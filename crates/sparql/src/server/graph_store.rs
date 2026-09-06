//! `/graphs` — the SPARQL 1.1 Graph Store Protocol (SPEC-28 S5).
//!
//! Four routes over one graph selected by `?graph=<iri>` or `?default`:
//! `GET` serializes it, `PUT` replaces it, `POST` merges into it, `DELETE`
//! empties it.
//!
//! Three rules the spec is emphatic about, all enforced here:
//!
//! * **`PUT` is a diff over base quads only.** The read set is
//!   [`Store::scan_graph_quads`] — asserted quads straight out of storage, no
//!   reasoning seam — so a derived quad is never deleted by a `PUT`
//!   (SPEC-29 D5). An empty diff commits nothing.
//! * **Reserved graphs are read-only.** `GET` of a
//!   `https://horndb.io/graph/…` graph is allowed; the three write verbs
//!   reuse Update's closed-namespace check
//!   ([`crate::update::reserved_iri_write_check`]).
//! * **`?default` is refused on a `--materialize` store** for the write
//!   verbs: `load_with_reasoning` puts asserted and inferred triples in the
//!   default graph indistinguishably, so a `PUT` diff would delete
//!   inferences the client never sent.
//!
//! Blank nodes are request-scoped (each request parses under a fresh
//! `next_bnode_doc_tag`), so re-`PUT`ting an identical bnode-bearing body
//! deletes and re-inserts every bnode-touching quad. That is the protocol's
//! requirement, not a bug: the empty-diff no-op cannot apply there.

use super::AppState;
use crate::exec::horn::algebra_to_oxrdf;
use crate::exec::{AlgebraQuad, AlgebraTriple, FullBackend};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use oxrdf::{NamedNode, NamedOrBlankNode, Term as OxTerm, Triple};
use spargebra::algebra::GraphTarget;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Which graph the request names. `Default` is `?default`, `Named` is
/// `?graph=<iri>`.
enum Target {
    Default,
    Named(String),
}

impl Target {
    /// `iri` was already validated by [`target`], the only place a `Named`
    /// value is built — `new_unchecked` here just avoids re-parsing it.
    fn graph_target(&self) -> GraphTarget {
        match self {
            Target::Default => GraphTarget::DefaultGraph,
            Target::Named(iri) => GraphTarget::NamedNode(NamedNode::new_unchecked(iri.clone())),
        }
    }

    /// The graph slot [`Store::apply_quads`] takes (`None` = default graph).
    fn graph_name(&self) -> Option<String> {
        match self {
            Target::Default => None,
            Target::Named(iri) => Some(iri.clone()),
        }
    }

    /// Base IRI for resolving relative IRIs in a Turtle payload, and for
    /// labelling parse errors. The graph's own IRI is the natural base; the
    /// default graph has none, so it gets a stable URN.
    fn base_iri(&self) -> &str {
        match self {
            Target::Default => "urn:horndb:default-graph",
            Target::Named(iri) => iri,
        }
    }
}

/// Boxed so the `Result`s below stay small — `axum::Response` is 128 bytes,
/// which `clippy::result_large_err` (rightly) objects to in an `Err` variant.
/// Same shape as `server::update`'s header check.
fn bad_request(msg: impl Into<String>) -> Box<Response> {
    Box::new((StatusCode::BAD_REQUEST, format!("{}\n", msg.into())).into_response())
}

/// Resolve `?graph=<iri>` / `?default`. Anything else — an unknown
/// parameter, neither, or both — is a 400 (SPEC-28 S5). A `graph` value that
/// is not a valid IRI is also a 400, with the parser's message in the body —
/// otherwise it would be interned as-is (`Target::graph_target` uses the
/// unchecked constructor) and could fail later, when a `GET` tries to
/// serialize it out as Turtle or N-Triples.
fn target(params: &HashMap<String, String>) -> Result<Target, Box<Response>> {
    let mut graph = None;
    let mut default = false;
    for (k, v) in params {
        match k.as_str() {
            "graph" => graph = Some(v.clone()),
            "default" => default = true,
            other => return Err(bad_request(format!("unknown query parameter `{other}`"))),
        }
    }
    match (graph, default) {
        (Some(g), false) => {
            NamedNode::new(&g).map_err(|e| bad_request(e.to_string()))?;
            Ok(Target::Named(g))
        }
        (None, true) => Ok(Target::Default),
        (None, false) => Err(bad_request("one of `graph=<iri>` or `default` is required")),
        (Some(_), true) => Err(bad_request("`graph` and `default` are mutually exclusive")),
    }
}

/// Refuse the write verbs where SPEC-28 S5 says they must be refused:
/// reserved graphs always, `?default` on a `--materialize` store.
fn check_writable(t: &Target) -> Result<(), Box<Response>> {
    match t {
        Target::Named(iri) => {
            crate::update::reserved_iri_write_check(iri).map_err(|e| bad_request(e.to_string()))
        }
        Target::Default if super::is_materialized() => Err(bad_request(
            "GSP writes to ?default are refused on a --materialize store: the closure's \
             inferred triples share the default graph with the asserted ones, so a whole-graph \
             write would delete inferences the request never sent",
        )),
        Target::Default => Ok(()),
    }
}

/// The `parse_rdf_bytes` extension for a request `Content-Type`. `None` is a
/// 415 — including the dataset formats TriG and N-Quads, which carry a graph
/// slot the protocol has no room for. A missing `Content-Type` defaults to
/// Turtle, matching `parse_rdf_bytes`' own default.
fn payload_extension(headers: &HeaderMap) -> Option<&'static str> {
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match ctype.as_str() {
        "" | "text/turtle" => Some("ttl"),
        "application/n-triples" => Some("nt"),
        _ => None,
    }
}

/// Lower a stored triple to `oxrdf` for serialization. `None` for a triple
/// the RDF abstract syntax cannot hold in those positions (a variable, or an
/// RDF 1.2 triple term), which the store never contains.
fn to_oxrdf_triple(t: &AlgebraTriple) -> Option<Triple> {
    let subject: NamedOrBlankNode = match algebra_to_oxrdf(&t.0).ok()? {
        OxTerm::NamedNode(n) => n.into(),
        OxTerm::BlankNode(b) => b.into(),
        _ => return None,
    };
    let predicate: NamedNode = match algebra_to_oxrdf(&t.1).ok()? {
        OxTerm::NamedNode(n) => n,
        _ => return None,
    };
    Some(Triple::new(
        subject,
        predicate,
        algebra_to_oxrdf(&t.2).ok()?,
    ))
}

/// Serialize `triples` per the `Accept` header. Content negotiation is over
/// `text/turtle` (the default) and `application/n-triples`.
fn serialize(triples: &[AlgebraTriple], headers: &HeaderMap) -> (&'static str, Vec<u8>) {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ox: Vec<Triple> = triples.iter().filter_map(to_oxrdf_triple).collect();
    if accept.contains("application/n-triples") {
        let mut w = oxttl::NTriplesSerializer::new().for_writer(Vec::new());
        for t in &ox {
            // Writing into a `Vec` cannot fail.
            let _ = w.serialize_triple(t);
        }
        ("application/n-triples", w.finish())
    } else {
        let mut w = oxttl::TurtleSerializer::new().for_writer(Vec::new());
        for t in &ox {
            let _ = w.serialize_triple(t);
        }
        ("text/turtle", w.finish().unwrap_or_default())
    }
}

/// The `PUT` diff: `dels = base − payload`, `adds = payload − base`. Split
/// out so the empty-diff rule is unit-testable without an HTTP round trip.
fn graph_diff(
    base: &HashSet<AlgebraTriple>,
    payload: &HashSet<AlgebraTriple>,
) -> (Vec<AlgebraTriple>, Vec<AlgebraTriple>) {
    (
        base.difference(payload).cloned().collect(),
        payload.difference(base).cloned().collect(),
    )
}

fn quads(graph: &Option<String>, triples: Vec<AlgebraTriple>) -> Vec<AlgebraQuad> {
    triples
        .into_iter()
        .map(|(s, p, o)| (graph.clone(), s, p, o))
        .collect()
}

/// Parse a request body into the triples it asserts, request-scoped blank
/// nodes and all. A parse failure is a 400 carrying the parser's message.
fn parse_payload<B: FullBackend + Send + Sync + 'static>(
    state: &AppState<B>,
    t: &Target,
    extension: &'static str,
    body: &[u8],
) -> Result<HashSet<AlgebraTriple>, Box<Response>> {
    let tag = state.store.read().next_bnode_doc_tag();
    match crate::update::parse_rdf_bytes(tag, body, Some(extension), t.base_iri()) {
        // A triples format never carries a graph slot, so the parsed graph
        // name is always the default-graph sentinel; drop it and re-tag with
        // the request's target.
        Ok(qs) => Ok(qs.into_iter().map(|(_, s, p, o)| (s, p, o)).collect()),
        Err(e) => Err(bad_request(e.to_string())),
    }
}

/// Run `f` with the store write lock held, off the async runtime workers —
/// same reasoning as `/update`: a tokio worker blocked in `write()` polls no
/// connections.
async fn with_write<B, F>(state: &AppState<B>, f: F) -> Response
where
    B: FullBackend + Send + Sync + 'static,
    F: FnOnce(&mut B) -> Response + Send + 'static,
{
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || f(&mut store.write())).await {
        Ok(resp) => resp,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "graph store task panicked\n",
        )
            .into_response(),
    }
}

fn executor_error(e: crate::error::SparqlError) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}\n")).into_response()
}

pub async fn handle_get<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = state.shed_while_loading() {
        return resp;
    }
    let t = match target(&params) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    // Reads of reserved graphs stay allowed (S5); only the write verbs are
    // closed. Admission control: a whole-graph scan is a store read of
    // unbounded size, gated like `/query` (HDB-118).
    let Some(permit) = state.admission.acquire().await else {
        let retry_after = state.admission.queue_timeout.as_secs().max(1).to_string();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", retry_after)],
            "server busy: no query slot available\n",
        )
            .into_response();
    };

    let store = Arc::clone(&state.store);
    let gt = t.graph_target();
    let scanned = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store.read().scan_graph_quads(&gt)
    })
    .await;

    let triples = match scanned {
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "graph store task panicked\n",
            )
                .into_response()
        }
        Ok(Err(e)) => return executor_error(e),
        Ok(Ok(ts)) => ts,
    };
    if triples.is_empty() {
        return (StatusCode::NOT_FOUND, "no such graph\n").into_response();
    }
    let (ctype, body) = serialize(&triples, &headers);
    ([("content-type", ctype)], body).into_response()
}

pub async fn handle_put<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_graph(state, params, headers, body, /* replace */ true).await
}

pub async fn handle_post<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_graph(state, params, headers, body, /* replace */ false).await
}

/// `PUT` (`replace`) and `POST` (merge) differ only in whether the diff
/// carries deletions.
async fn write_graph<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    params: HashMap<String, String>,
    headers: HeaderMap,
    body: Bytes,
    replace: bool,
) -> Response {
    if let Some(resp) = state.shed_while_loading() {
        return resp;
    }
    let t = match target(&params) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    if let Err(resp) = check_writable(&t) {
        return *resp;
    }
    let Some(extension) = payload_extension(&headers) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported media type: the Graph Store Protocol serves one graph, so only the \
             triples formats text/turtle and application/n-triples are accepted\n",
        )
            .into_response();
    };
    let payload = match parse_payload(&state, &t, extension, &body) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let gt = t.graph_target();
    let graph = t.graph_name();
    with_write(&state, move |store| {
        let base: HashSet<AlgebraTriple> = match store.scan_graph_quads(&gt) {
            Ok(ts) => ts.into_iter().collect(),
            Err(e) => return executor_error(e),
        };
        let (dels, adds) = graph_diff(&base, &payload);
        let dels = if replace {
            quads(&graph, dels)
        } else {
            Vec::new()
        };
        let adds = quads(&graph, adds);
        let created = base.is_empty() && !adds.is_empty();
        // An empty diff commits nothing (S5) — no batch, no delta.
        if !dels.is_empty() || !adds.is_empty() {
            if let Err(e) = store.apply_quads(dels, adds) {
                return executor_error(e);
            }
        }
        if created {
            StatusCode::CREATED.into_response()
        } else {
            StatusCode::NO_CONTENT.into_response()
        }
    })
    .await
}

pub async fn handle_delete<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = state.shed_while_loading() {
        return resp;
    }
    let t = match target(&params) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    if let Err(resp) = check_writable(&t) {
        return *resp;
    }
    let gt = t.graph_target();
    with_write(&state, move |store| match store.clear_graph(&gt) {
        Err(e) => executor_error(e),
        // D11: a graph exists iff it holds a quad, so clearing an empty one
        // is a 404, not an idempotent 204.
        Ok(0) => (StatusCode::NOT_FOUND, "no such graph\n").into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Term;

    fn t(s: &str) -> AlgebraTriple {
        (
            Term::Iri(format!("http://ex/{s}")),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        )
    }

    #[test]
    fn identical_payload_diffs_to_nothing() {
        let base: HashSet<AlgebraTriple> = [t("a"), t("b")].into_iter().collect();
        let (dels, adds) = graph_diff(&base, &base.clone());
        assert!(dels.is_empty() && adds.is_empty());
    }

    #[test]
    fn diff_is_symmetric_difference() {
        let base: HashSet<AlgebraTriple> = [t("a"), t("b")].into_iter().collect();
        let payload: HashSet<AlgebraTriple> = [t("b"), t("c")].into_iter().collect();
        let (dels, adds) = graph_diff(&base, &payload);
        assert_eq!(dels, vec![t("a")]);
        assert_eq!(adds, vec![t("c")]);
    }

    #[test]
    fn dataset_formats_are_unsupported_media_types() {
        let mut h = HeaderMap::new();
        for ct in [
            "application/trig",
            "application/n-quads",
            "application/json",
        ] {
            h.insert("content-type", ct.parse().unwrap());
            assert!(payload_extension(&h).is_none(), "{ct} must be a 415");
        }
        h.insert("content-type", "text/turtle;charset=utf-8".parse().unwrap());
        assert_eq!(payload_extension(&h), Some("ttl"));
        h.insert("content-type", "application/n-triples".parse().unwrap());
        assert_eq!(payload_extension(&h), Some("nt"));
    }
}
