//! `/query` HTTP handler. Per SPARQL 1.1 Protocol:
//!   * GET with `query` in the URL query string,
//!   * POST `application/sparql-query` raw,
//!   * POST `application/x-www-form-urlencoded` with `query=`.

use super::stream_body::ChannelBody;
use super::AppState;
use crate::api::{execute_query, plan_select, QueryAnswer};
use crate::error::SparqlError;
use crate::exec::runtime::Runtime;
use crate::exec::FullBackend;
use crate::plan::PhysicalPlan;
use crate::results::{
    csv::write_select_csv, json::write_ask_json, json::write_select_json, select_serializer,
    tsv::write_select_tsv, xml::write_ask_xml, xml::write_select_xml, ResultFormat,
};
use crate::SparqlConfig;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use horndb_metrics::labels::{Stage, StageLabel};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

#[derive(Deserialize)]
pub struct QueryParams {
    pub query: Option<String>,
}

pub async fn handle_query_get<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(p): Query<QueryParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(q) = p.query else {
        return (
            StatusCode::BAD_REQUEST,
            "missing `query` parameter".to_string(),
        )
            .into_response();
    };
    run(state, &q, &headers).await
}

pub async fn handle_query_post<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Per the protocol, `application/x-www-form-urlencoded` carries
    // a `query=` field; `application/sparql-query` is raw. We sniff.
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let query = if ctype.contains("application/x-www-form-urlencoded") {
        match url_form_field(&body, "query") {
            Some(q) => q,
            None => {
                return (StatusCode::BAD_REQUEST, "form missing `query`".to_string())
                    .into_response();
            }
        }
    } else {
        body
    };
    run(state, &query, &headers).await
}

/// Extract a single urlencoded form field by key (`query=…` / `update=…`)
/// from a request body, percent-decoding its value.
pub(crate) fn url_form_field(body: &str, key: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == key {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal decoder — sufficient for tests. `urlencoding` crate
    // would be the prod choice; avoid the dep in Stage 1.
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn run<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    q: &str,
    headers: &HeaderMap,
) -> axum::response::Response {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let fmt = ResultFormat::from_accept(accept);

    // Plain SELECTs stream; everything else (ASK / CONSTRUCT / DESCRIBE /
    // EXPLAIN) keeps the materialized path — their results are small.
    // Planning needs no store access, so it runs here on the async thread.
    match plan_select(q, &SparqlConfig::default()) {
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Ok(Some((vars, plan))) => stream_select(state, vars, plan, fmt).await,
        Ok(None) => run_materialized(state, q, fmt).await,
    }
}

/// Serialized chunks buffered between the blocking serializer thread and
/// the async body. Bounded: a slow client exerts backpressure on the
/// executor instead of buffering the whole result.
const STREAM_CHANNEL_CHUNKS: usize = 8;

/// Mirror `api::timed(Stage::Exec, …)` for the streaming path: observe the
/// stage duration (here: time to first chunk) and bump `query_errors` on
/// error. Note `request_duration_seconds` also stops at response headers —
/// roughly the same instant — so no duration metric covers the full body
/// drain; only `response_bytes` (via `CountingBody`) reflects delivered
/// bytes.
fn record_exec(start: Instant, err: bool) {
    let m = horndb_metrics::metrics();
    let label = StageLabel { stage: Stage::Exec };
    m.sparql
        .stage_duration_seconds
        .get_or_create(&label)
        .observe(start.elapsed().as_secs_f64());
    if err {
        m.sparql.query_errors.get_or_create(&label).inc();
    }
}

/// Bump `query_errors{stage=exec}` for an error after the exec stage was
/// already observed (mid-stream failure).
fn bump_exec_error() {
    horndb_metrics::metrics()
        .sparql
        .query_errors
        .get_or_create(&StageLabel { stage: Stage::Exec })
        .inc();
}

/// First reply from the blocking executor: either the whole document
/// (result fit in one chunk — reply as a plain sized body) or the
/// pre-buffered head of a multi-chunk stream.
enum FirstReply {
    Complete(String),
    Streaming(Bytes),
}

/// Execute + decode + serialize a SELECT on a blocking thread, streaming
/// serialized `Bytes` chunks to the response body over a bounded channel.
///
/// Everything store-touching stays on the one blocking thread: the
/// `RwLockReadGuard` and the operator tree (`Box<dyn Op>`, which borrows
/// through the guard) are `!Send`. The first chunk is decoded BEFORE any
/// bytes are emitted, so build/scan/first-decode errors return a clean 400;
/// after that, an error aborts the chunked body (see `ChannelBody`).
///
/// Fast path: when the result fits in a single operator chunk (including
/// the empty result), the whole document is returned as a plain sized body
/// (Content-Length, one frame) instead of a chunked channel body. The
/// chunk-2 peek that detects this happens before headers commit, so a clean
/// first chunk still commits a 200 even if the peek errors.
///
/// Trade-off (accepted, see the 2026-07-06 design spec): the read lock is
/// held until the client drains the response, so a slow download blocks
/// writers (not readers). SPEC-02 MVCC removes this; the bounded channel
/// plus the send-failure-on-disconnect path bound the damage a dead client
/// can do.
async fn stream_select<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    vars: Vec<String>,
    plan: PhysicalPlan,
    fmt: ResultFormat,
) -> axum::response::Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, SparqlError>>(STREAM_CHANNEL_CHUNKS);
    let (first_tx, first_rx) = oneshot::channel::<Result<FirstReply, SparqlError>>();
    let store = Arc::clone(&state.store);

    tokio::task::spawn_blocking(move || {
        let store = store.read().unwrap();
        let rt = Runtime::new(&*store);
        let mut ser = select_serializer(fmt);
        let start = Instant::now();

        let mut stream = match rt.run_stream(&plan) {
            Ok(s) => s,
            Err(e) => {
                record_exec(start, true);
                let _ = first_tx.send(Err(e));
                return;
            }
        };
        // Pre-buffer chunk 1 so its errors surface before headers commit.
        let first_rows = match stream.next_chunk() {
            Ok(r) => r,
            Err(e) => {
                record_exec(start, true);
                let _ = first_tx.send(Err(e));
                return;
            }
        };
        record_exec(start, false);

        let mut head = ser.header(&vars);
        match first_rows {
            Some(rows) => head.push_str(&ser.chunk(&vars, &rows)),
            None => {
                // Empty result: a sized body carrying the whole document.
                head.push_str(&ser.footer());
                let _ = first_tx.send(Ok(FirstReply::Complete(head)));
                return;
            }
        }
        // Peek chunk 2: if the first chunk was the last, reply with the
        // complete document as a sized body (fast path — no channel body).
        let second_rows = match stream.next_chunk() {
            Ok(r) => r,
            Err(e) => {
                // Chunk 1 was clean, so headers must still commit (200)
                // and the error must abort the body mid-stream — exactly
                // the pre-fast-path contract (see ChannelBody).
                let _ = first_tx.send(Ok(FirstReply::Streaming(Bytes::from(head))));
                bump_exec_error();
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        let rows2 = match second_rows {
            Some(rows) => rows,
            None => {
                head.push_str(&ser.footer());
                let _ = first_tx.send(Ok(FirstReply::Complete(head)));
                return;
            }
        };
        // Multi-chunk: commit the streaming path, then forward chunk 2.
        if first_tx
            .send(Ok(FirstReply::Streaming(Bytes::from(head))))
            .is_err()
        {
            return; // client disconnected — release the read lock
        }
        if tx
            .blocking_send(Ok(Bytes::from(ser.chunk(&vars, &rows2))))
            .is_err()
        {
            return; // client disconnected
        }
        loop {
            match stream.next_chunk() {
                Ok(Some(rows)) => {
                    let bytes = Bytes::from(ser.chunk(&vars, &rows));
                    if tx.blocking_send(Ok(bytes)).is_err() {
                        return; // client disconnected
                    }
                }
                Ok(None) => {
                    let _ = tx.blocking_send(Ok(Bytes::from(ser.footer())));
                    return;
                }
                Err(e) => {
                    // Headers are committed: abort the body (see ChannelBody).
                    bump_exec_error();
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });

    match first_rx.await {
        // Whole result fit in one chunk: plain sized body, same shape as
        // `run_materialized`'s Solutions arm.
        Ok(Ok(FirstReply::Complete(body))) => {
            (StatusCode::OK, [("content-type", fmt.content_type())], body).into_response()
        }
        Ok(Ok(FirstReply::Streaming(first))) => {
            let body = axum::body::Body::new(ChannelBody::new(first, rx));
            (StatusCode::OK, [("content-type", fmt.content_type())], body).into_response()
        }
        // Errors before any byte was emitted are still a clean 400 —
        // parity with the materialized path's error handling.
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "result stream ended before producing output".to_string(),
        )
            .into_response(),
    }
}

/// Materialized path for non-SELECT forms (and the pre-streaming behavior):
/// execute fully, then serialize in one shot. Body identical to the old
/// `run` except `fmt` is passed in.
async fn run_materialized<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    q: &str,
    fmt: ResultFormat,
) -> axum::response::Response {
    // Scope the read guard to the execution only; results are
    // materialised into `ans`, so serialization below holds no lock and
    // never blocks a concurrent writer.
    let ans = {
        let store = state.store.read().unwrap();
        match execute_query(q, &*store) {
            Ok(a) => a,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
        }
    };

    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            // Unreachable for plain SELECTs (they take stream_select), but
            // kept for defense in depth — behavior is identical.
            let body = match fmt {
                ResultFormat::Json => write_select_json(&vars, &rows),
                ResultFormat::Xml => write_select_xml(&vars, &rows),
                ResultFormat::Csv => write_select_csv(&vars, &rows),
                ResultFormat::Tsv => write_select_tsv(&vars, &rows),
            };
            (StatusCode::OK, [("content-type", fmt.content_type())], body).into_response()
        }
        QueryAnswer::Boolean(b) => {
            // CSV/TSV have no boolean serialisation; fall back to XML
            // (the protocol default for ASK in many clients) for those.
            let (ctype, body) = match fmt {
                ResultFormat::Json => (ResultFormat::Json.content_type(), write_ask_json(b)),
                _ => (ResultFormat::Xml.content_type(), write_ask_xml(b)),
            };
            (StatusCode::OK, [("content-type", ctype)], body).into_response()
        }
        QueryAnswer::Triples(triples) => {
            // Stage 1: serialise CONSTRUCT as N-Triples.
            let mut s = String::new();
            for (sub, p, o) in triples {
                s.push_str(&format!("<{sub}> <{p}> <{o}> .\n"));
            }
            (
                StatusCode::OK,
                [("content-type", "application/n-triples")],
                s,
            )
                .into_response()
        }
        QueryAnswer::Explanation { text, json } => {
            // EXPLAIN (SPEC-07 F9): the plan rendering. The format is
            // fixed by the pragma (`EXPLAIN` vs `EXPLAIN JSON`), not the
            // Accept header, since EXPLAIN output is not a SPARQL results
            // document.
            let ctype = if json {
                "application/json"
            } else {
                "text/plain; charset=utf-8"
            };
            (StatusCode::OK, [("content-type", ctype)], text).into_response()
        }
    }
}
