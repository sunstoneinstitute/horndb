//! `sparql11-gsp` — grade a W3C Graph Store Protocol case against a live
//! HornDB server (SPEC-28 S5).
//!
//! Every other suite in this harness grades files. These cases are HTTP
//! request/response pairs, and the state one request leaves behind is what
//! the next request asserts on, so the whole sequence runs against one
//! server. We boot the real axum router on `127.0.0.1:0` — port 0, so the
//! kernel assigns a free port and parallel runs cannot collide — and speak
//! HTTP/1.1 to it over a plain TCP socket.
//!
//! The socket is hand-rolled rather than pulled from an HTTP client crate:
//! each message is one request line, a few headers and an optional body, and
//! `Connection: close` makes the response "read to EOF", so there is no
//! chunked-transfer or keep-alive framing to implement.
//!
//! Two mappings the upstream manifest asks a runner to make:
//!
//! * **Endpoint prefix.** Every `ht:absolutePath` starts with `/gsp`; the
//!   manifest says to substitute the endpoint under test. HornDB's is
//!   [`ENDPOINT`]. A *direct*-identification case (`/gsp/person/1.ttl`, the
//!   graph named by the request path) therefore becomes `/graphs/person/1.ttl`
//!   and 404s — HornDB only does indirect identification, `?graph=<iri>`.
//! * **Response bodies** are compared as graphs, not bytes: both sides are
//!   parsed and blank-node-canonicalized, which is the isomorphism the
//!   manifest's own prose asks for.

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};
use oxrdf::graph::CanonicalizationAlgorithm;
use oxrdf::Graph;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::testcase::GspRequest;

/// HornDB's Graph Store Protocol route (SPEC-28 S5), substituted for the
/// manifest's `/gsp` prefix.
const ENDPOINT: &str = "/graphs";

/// Run one case. `Ok(None)` is a pass; `Ok(Some(reason))` a failure naming
/// the request that broke. An error is a harness fault (could not bind, etc.).
pub fn run_case(requests: &[GspRequest]) -> Result<Option<String>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime for the GSP server")?
        .block_on(drive(requests))
}

async fn drive(requests: &[GspRequest]) -> Result<Option<String>> {
    // `HornBackend`, not `MemStore`: the toy store keeps only a lexical form
    // per term, so a blank node comes back out of it as an IRI. The Graph
    // Store Protocol cases are full of blank nodes, and `HornBackend` is the
    // storage path the real server uses (same choice as `sparql_eval.rs`).
    use horndb_sparql::exec::horn::HornBackend;
    use horndb_sparql::server::{build_router, AppState, Limits};

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the GSP test server")?;
    let addr = listener.local_addr()?;
    let app = build_router(AppState {
        store: Arc::new(parking_lot::RwLock::new(HornBackend::new())),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Limits::default(),
    });
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let mut verdict = None;
    for (i, req) in requests.iter().enumerate() {
        match check(addr, req).await {
            Ok(None) => {}
            Ok(Some(why)) => {
                verdict = Some(format!(
                    "request {} ({} {}): {why}",
                    i + 1,
                    req.method,
                    req.path
                ));
                break;
            }
            Err(e) => {
                verdict = Some(format!(
                    "request {} ({} {}): transport error: {e:#}",
                    i + 1,
                    req.method,
                    req.path
                ));
                break;
            }
        }
    }
    server.abort();
    Ok(verdict)
}

/// Send one request and grade its response. `Ok(None)` means it matched.
async fn check(addr: SocketAddr, req: &GspRequest) -> Result<Option<String>> {
    let path = req.path.replacen("/gsp", ENDPOINT, 1);
    let (status, content_type, body) = send(addr, req, &path).await?;

    if !req.expected_status.contains(&status) {
        return Ok(Some(format!(
            "got {status}, expected one of {:?} (body: {})",
            req.expected_status,
            body.trim()
        )));
    }
    let Some(expected) = &req.expected_body else {
        return Ok(None);
    };
    let want = match parse_graph(expected, req.expected_body_type.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            return Ok(Some(format!(
                "expected body in the manifest is unparsable: {e}"
            )))
        }
    };
    let got = match parse_graph(&body, Some(&content_type)) {
        Ok(g) => g,
        Err(e) => {
            return Ok(Some(format!(
                "response body is unparsable as {content_type}: {e}"
            )))
        }
    };
    if want == got {
        Ok(None)
    } else {
        Ok(Some(format!(
            "response graph is not isomorphic to the expected one ({} vs {} triples)",
            got.len(),
            want.len()
        )))
    }
}

/// One request over one connection. Returns `(status, content-type, body)`.
async fn send(addr: SocketAddr, req: &GspRequest, path: &str) -> Result<(u16, String, String)> {
    let body = req.body.clone().unwrap_or_default();
    let mut wire = format!(
        "{} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        req.method,
        body.len()
    );
    for (name, value) in &req.headers {
        wire.push_str(&format!("{name}: {value}\r\n"));
    }
    wire.push_str("\r\n");
    wire.push_str(&body);

    let mut sock = TcpStream::connect(addr).await?;
    sock.write_all(wire.as_bytes()).await?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<(u16, String, String)> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("response has no header/body separator"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("malformed status line"))?;
    let content_type = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.trim().to_owned())
        .unwrap_or_default();
    Ok((status, content_type, body.to_owned()))
}

/// Parse an RDF payload into a canonicalized graph, so two graphs compare
/// equal exactly when they are isomorphic. The media type picks the parser;
/// anything that is not N-Triples is read as Turtle (which N-Triples is a
/// subset of anyway).
fn parse_graph(text: &str, media_type: Option<&str>) -> Result<Graph> {
    let mut graph = Graph::new();
    // Bodies in these manifests use absolute IRIs, but a base is still needed
    // for the parser to accept a relative one if upstream ever adds it.
    if media_type.is_some_and(|m| m.contains("n-triples")) {
        for t in oxttl::NTriplesParser::new().for_slice(text.as_bytes()) {
            graph.insert(&t?);
        }
    } else {
        for t in oxttl::TurtleParser::new()
            .with_base_iri("http://www.example/gsp/")?
            .for_slice(text.as_bytes())
        {
            graph.insert(&t?);
        }
    }
    graph.canonicalize(CanonicalizationAlgorithm::Unstable);
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_compare_up_to_blank_node_renaming() {
        let a = parse_graph("[] <http://ex/p> \"x\" .", None).unwrap();
        let b = parse_graph("_:other <http://ex/p> \"x\" .", None).unwrap();
        let c = parse_graph("[] <http://ex/p> \"y\" .", None).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    /// The end-to-end check: a real bound server answers a real socket, and
    /// the `/gsp` prefix in the manifest reaches HornDB's `/graphs` route.
    #[test]
    fn drives_a_live_server_through_a_put_get_sequence() {
        let put = GspRequest {
            method: "PUT".into(),
            path: "/gsp?graph=http%3A%2F%2Fex%2Fg".into(),
            headers: vec![("content-type".into(), "text/turtle".into())],
            body: Some("<http://ex/a> <http://ex/p> <http://ex/o> .".into()),
            expected_status: vec![201],
            expected_body: None,
            expected_body_type: None,
        };
        let get = GspRequest {
            method: "GET".into(),
            path: "/gsp?graph=http%3A%2F%2Fex%2Fg".into(),
            headers: vec![("accept".into(), "text/turtle".into())],
            body: None,
            expected_status: vec![200],
            expected_body: Some("<http://ex/a> <http://ex/p> <http://ex/o> .".into()),
            expected_body_type: Some("text/turtle".into()),
        };
        assert_eq!(run_case(&[put.clone(), get]).unwrap(), None);

        // A wrong expectation must actually fail, or the pass above is vacuous.
        let mut wrong = put;
        wrong.expected_status = vec![204];
        assert!(run_case(&[wrong]).unwrap().is_some());
    }
}
