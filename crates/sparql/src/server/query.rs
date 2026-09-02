//! `/query` HTTP handler. Per SPARQL 1.1 Protocol:
//!   * GET with `query` in the URL query string,
//!   * POST `application/sparql-query` raw,
//!   * POST `application/x-www-form-urlencoded` with `query=`.
//!
//! Every request also builds its own [`QuerySettings`] (SPEC-26 S4): the
//! server's `[server.limits]` defaults with the whitelisted URL/form
//! overrides layered on top. `query_timeout`, `max_result_rows` and `rdf12`
//! are enforced here (S5); `max_query_memory` is accepted and carried but
//! **not yet enforced** — see [`resolve_settings`].

use super::stream_body::ChannelBody;
use super::{AppState, QueryPermit};
use crate::algebra::DatasetSpec;
use crate::api::{execute_query_with, plan_select, QueryAnswer};
use crate::error::SparqlError;
use crate::exec::runtime::Runtime;
use crate::exec::FullBackend;
use crate::plan::PhysicalPlan;
use crate::results::{
    csv::write_select_csv, json::write_ask_json, json::write_select_json, select_serializer,
    tsv::write_select_tsv, xml::write_ask_xml, xml::write_select_xml, ResultFormat,
};
use crate::{DefaultGraphMode, SparqlConfig};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use horndb_config::QuerySettings;
use horndb_metrics::labels::{Stage, StageLabel};
use horndb_wcoj::cancel::CancelToken;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// URL/form keys on `/query` that are **not** SPEC-26 S4 overrides.
/// `query` is the query text itself; the two graph-uri keys are reserved by
/// the SPARQL 1.1 Protocol (SPEC-28 phase 5's Graph Store Protocol needs
/// them on this endpoint) and are still ignored here — but they must not be
/// mistaken for an unknown setting and rejected. Every *other* key is either
/// a whitelisted override or a 400.
const PROTOCOL_KEYS: [&str; 3] = ["query", "default-graph-uri", "named-graph-uri"];

/// Params come back as raw pairs rather than a typed struct: SPEC-26 S4
/// needs the whole key set to tell a whitelisted override from an unknown
/// key, which a `#[derive(Deserialize)]` struct would silently drop.
type Params = Vec<(String, String)>;

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// SPEC-26 S4: build one query's settings — the server's `[server.limits]`
/// defaults with each `layers` entry applied on top, later layers winning.
///
/// Only the [`QuerySettings`] whitelist is overridable; an unknown key or an
/// unparseable value is `Err` (a per-query 400 naming the key) and touches
/// neither the server config nor any other query. Layering is a fold over an
/// ordered list precisely so a future session tier slots in as one more
/// layer between the defaults and the URL params.
///
/// Keys are spelled after their `QuerySettings` field (`query_timeout`,
/// `default_graph`, …), never kebab-case — `default-graph` would sit one
/// suffix from the protocol's reserved `default-graph-uri` above.
///
/// **`max_query_memory` is accepted, parsed and carried, but not enforced**
/// (SPEC-26 S5, non-goal): real per-query memory accounting is the companion
/// spec's. Setting it bounds nothing today.
fn resolve_settings(
    limits: &horndb_config::Limits,
    layers: &[&[(String, String)]],
) -> Result<QuerySettings, String> {
    let mut settings = QuerySettings::from_limits(limits);
    for layer in layers {
        for (key, value) in layer.iter() {
            if PROTOCOL_KEYS.contains(&key.as_str()) {
                continue;
            }
            settings.apply_override(key, value)?;
        }
    }
    Ok(settings)
}

pub async fn handle_query_get<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(q) = param(&params, "query").map(str::to_owned) else {
        return (
            StatusCode::BAD_REQUEST,
            "missing `query` parameter".to_string(),
        )
            .into_response();
    };
    let settings = match resolve_settings(&state.limits, &[&params]) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    run(state, &q, &headers, settings).await
}

pub async fn handle_query_post<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Per the protocol, `application/x-www-form-urlencoded` carries
    // a `query=` field; `application/sparql-query` is raw. We sniff.
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // A form-encoded POST can carry overrides in both channels; the body
    // wins, with the URL query string as the fallback for a client that put
    // them there instead — the precedence `query=` itself already implies.
    // A direct POST's raw body IS the query text (SPARQL 1.1 Protocol
    // §2.1.2), so there the overrides travel on the URL, like GET's.
    let (query, body_params) = if ctype.contains("application/x-www-form-urlencoded") {
        let params = url_form_pairs(&body);
        let Some(q) = param(&params, "query").map(str::to_owned) else {
            return (StatusCode::BAD_REQUEST, "form missing `query`".to_string()).into_response();
        };
        (q, params)
    } else {
        (body, Params::new())
    };
    let settings = match resolve_settings(&state.limits, &[&params, &body_params]) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    run(state, &query, &headers, settings).await
}

/// Split an urlencoded body (`query=…&max_result_rows=…`) into its
/// percent-decoded key/value pairs, in order. One parser for both the form
/// body and, via [`url_form_field`], `/update`'s single field.
pub(crate) fn url_form_pairs(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some(k), Some(v)) => Some((percent_decode(k), percent_decode(v))),
                _ => None,
            }
        })
        .collect()
}

/// Extract a single urlencoded form field by key (`update=…`) from a request
/// body. A duplicate form key takes its first occurrence — a pre-existing
/// asymmetry with the URL `Query` extractor, which rejects duplicates.
pub(crate) fn url_form_field(body: &str, key: &str) -> Option<String> {
    url_form_pairs(body)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
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
    settings: QuerySettings,
) -> axum::response::Response {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let fmt = ResultFormat::from_accept(accept);

    // The two settings the SPARQL pipeline itself consumes (SPEC-26 S5:
    // `rdf12` per query is what makes the already-plumbed per-request
    // `SparqlConfig` path live from the HTTP layer).
    let cfg = SparqlConfig {
        rdf12: settings.rdf12,
        default_graph: settings.default_graph.into(),
    };

    // HDB-118 admission control: take an execution slot before touching the
    // store. Acquired here (not deeper) so both the streaming and the
    // materialized path are covered by one gate.
    let Some(permit) = state.admission.acquire().await else {
        let retry_after = state.admission.queue_timeout.as_secs().max(1).to_string();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", retry_after)],
            "server busy: no query slot available\n".to_string(),
        )
            .into_response();
    };

    // Plain SELECTs stream; everything else (ASK / CONSTRUCT / DESCRIBE /
    // EXPLAIN) keeps the materialized path — their results are small.
    // Planning needs no store access, so it runs here on the async thread.
    match plan_select(q, &cfg) {
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        Ok(Some((vars, plan, dataset))) => {
            stream_select(
                state,
                q,
                vars,
                plan,
                dataset,
                cfg.default_graph,
                fmt,
                permit,
                &settings,
            )
            .await
        }
        Ok(None) => run_materialized(state, q, fmt, &cfg, permit, &settings).await,
    }
}

/// SPEC-26 S5: arm this query's `query_timeout`.
///
/// Returns the [`CancelToken`] to publish to the executors plus a
/// "still running" sender: the timer cancels only if that sender is still
/// alive at the deadline, so a query that finishes first leaves no task
/// sleeping out the rest of its timeout. The timer lives here, in the
/// server layer — `horndb-wcoj` only ever sees a plain `CancelToken` and
/// gains no config dependency.
fn arm_timeout(timeout: Duration) -> (CancelToken, oneshot::Sender<()>) {
    let token = CancelToken::new();
    let (running_tx, running_rx) = oneshot::channel::<()>();
    let timer_token = token.clone();
    tokio::spawn(async move {
        // Err == the deadline hit first; Ok == the query ended and dropped
        // its sender.
        if tokio::time::timeout(timeout, running_rx).await.is_err() {
            timer_token.cancel();
        }
    });
    (token, running_tx)
}

/// Status for an error that lands *before* any body byte. A timeout is the
/// server giving up, not a malformed request, so it is not a 400.
fn error_status(e: &SparqlError) -> StatusCode {
    match e {
        SparqlError::QueryTimeout => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_REQUEST,
    }
}

/// Re-label an execution error raised after the query was cancelled: the
/// executor reports its own "cancelled", the client needs the reason.
fn classify(e: SparqlError, cancel: &CancelToken) -> SparqlError {
    if cancel.is_cancelled() {
        SparqlError::QueryTimeout
    } else {
        e
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
///
/// HDB-99: also flushes this thread's accumulated per-operator exec phases
/// (`HORNDB_EXEC_PHASES=1`). Since the elapsed time measured here stops at
/// the first chunk, the phase split it produces covers the same "get the
/// first chunk out" window, not the full result-set drain — see
/// `docs/metrics.md`.
fn record_exec(start: Instant, err: bool) {
    let m = horndb_metrics::metrics();
    let label = StageLabel { stage: Stage::Exec };
    let elapsed = start.elapsed();
    m.sparql
        .stage_duration_seconds
        .get_or_create(&label)
        .observe(elapsed.as_secs_f64());
    if err {
        m.sparql.query_errors.get_or_create(&label).inc();
    }
    crate::exec::phases::flush(elapsed);
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

/// Aborts the response body if the blocking serializer unwinds. Without
/// it a panic just drops `tx`, which ends the chunked body *cleanly* — the
/// client gets a well-formed short document and HTTP 200 with no signal
/// that rows are missing (undetectable for CSV/TSV). Sending `Err` makes
/// `ChannelBody` abort instead, so the truncation shows up at the protocol
/// level like every other mid-stream failure.
///
/// The panic payload/backtrace still goes to stderr via the default panic
/// hook; this only adds the identifying query (the server has no per-query
/// id to log) and the abort. Nothing here may panic — a panic during
/// unwind aborts the process.
struct AbortBodyOnPanic {
    tx: mpsc::Sender<Result<Bytes, SparqlError>>,
    /// Truncated at construction: never do fallible work during unwind.
    query: String,
}

impl Drop for AbortBodyOnPanic {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        eprintln!(
            "error: streaming SELECT serializer panicked; aborting response body. query: {}",
            self.query
        );
        bump_exec_error();
        let _ = self.tx.blocking_send(Err(SparqlError::Executor(
            "streaming serializer panicked".into(),
        )));
    }
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
/// The store read lock is held only long enough to pin a read view
/// (`Pinnable::pin_read`, HDB-119); execution and streaming run with no lock
/// held, so a slow client can no longer block `/update`. The view is a
/// point-in-time snapshot: an update committed while the body streams is
/// invisible to this query, whatever it commits.
///
/// Everything store-touching stays on the one blocking thread: the operator
/// tree (`Box<dyn Op>`, which borrows the view) is `!Send`. The first chunk
/// is decoded BEFORE any bytes are emitted, so build/scan/first-decode
/// errors return a clean 400; after that, an error aborts the chunked body
/// (see `ChannelBody`).
///
/// Fast path: when the result fits in a single operator chunk (including
/// the empty result), the whole document is returned as a plain sized body
/// (Content-Length, one frame) instead of a chunked channel body. The
/// chunk-2 peek that detects this happens before headers commit, so a clean
/// first chunk still commits a 200 even if the peek errors.
#[allow(clippy::too_many_arguments)]
async fn stream_select<B: FullBackend + Send + Sync + 'static>(
    state: AppState<B>,
    query: &str,
    vars: Vec<String>,
    plan: PhysicalPlan,
    dataset: DatasetSpec,
    default_graph: DefaultGraphMode,
    fmt: ResultFormat,
    permit: QueryPermit,
    settings: &QuerySettings,
) -> axum::response::Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, SparqlError>>(STREAM_CHANNEL_CHUNKS);
    let (first_tx, first_rx) = oneshot::channel::<Result<FirstReply, SparqlError>>();
    let store = Arc::clone(&state.store);
    let (cancel, running) = arm_timeout(settings.query_timeout.0);
    let max_rows = settings.max_result_rows;

    // Declared first inside the closure so it drops LAST — after the pinned
    // read view and the operator tree — and therefore stays armed for every
    // line that can panic.
    let guard = AbortBodyOnPanic {
        tx: tx.clone(),
        query: query.chars().take(200).collect(),
    };

    tokio::task::spawn_blocking(move || {
        let _abort_on_panic = guard;
        // HDB-118: the permit moves in here and is dropped when this closure
        // returns — i.e. it is held for the WHOLE stream, not just plan+first
        // chunk. That is deliberate: this task owns a blocking-pool thread,
        // a pinned read view and the operator tree for as long as the client
        // is draining, so releasing at first chunk would cap nothing. Since
        // HDB-119 no store lock is among them, but the thread and the pin
        // still are.
        // Every exit path below returns from the closure (clean finish,
        // client disconnect, error) and a panic unwinds it, so the slot is
        // freed exactly once.
        let _permit = permit;
        // SPEC-26 S5. `running` disarms the timeout on every return path
        // below; `cancel::scope` publishes the token to the executors on
        // this thread (and clears it again on the way out, so the next
        // query to be scheduled here does not inherit this one's cancel).
        let _running = running;
        let _cancel_scope = crate::exec::cancel::scope(cancel.clone());
        // Solutions serialized so far, against `max_result_rows`. The cap
        // NEVER truncates: the response ends with a typed
        // `ResultRowLimit` error instead (a 400 while the headers are
        // still uncommitted, an aborted body after).
        let mut emitted: u64 = 0;
        // HDB-99: discard any per-operator exec-phase data left over on this
        // (tokio blocking-pool, thread-reused) thread from a previous
        // query's trailing chunks — see `exec::phases::reset`. This query's
        // own `record_exec` flush only covers up to the first chunk; the
        // reset here is what stops a query's later, un-flushed chunk work
        // from being silently attributed to whichever query flushes next
        // on this thread.
        crate::exec::phases::reset();
        // The only lock this handler takes, and only for the pin itself.
        let view = {
            let store = store.read();
            store.pin_read()
        };
        let rt = Runtime::new(&view).with_dataset(dataset, default_graph);
        let mut ser = select_serializer(fmt);
        let start = Instant::now();

        let mut stream = match rt.run_stream(&plan) {
            Ok(s) => s,
            Err(e) => {
                record_exec(start, true);
                let _ = first_tx.send(Err(classify(e, &cancel)));
                return;
            }
        };
        // Pre-buffer chunk 1 so its errors surface before headers commit.
        let first_rows = match stream.next_chunk() {
            Ok(r) => r,
            Err(e) => {
                record_exec(start, true);
                let _ = first_tx.send(Err(classify(e, &cancel)));
                return;
            }
        };
        record_exec(start, false);

        let mut head = ser.header(&vars);
        match first_rows {
            Some(rows) => {
                emitted += rows.len() as u64;
                if emitted > max_rows {
                    let _ = first_tx.send(Err(SparqlError::ResultRowLimit(max_rows)));
                    return;
                }
                head.push_str(&ser.chunk(&vars, &rows))
            }
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
                let _ = tx.blocking_send(Err(classify(e, &cancel)));
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
        emitted += rows2.len() as u64;
        if emitted > max_rows {
            // Still uncommitted: a clean typed 400, no partial document.
            let _ = first_tx.send(Err(SparqlError::ResultRowLimit(max_rows)));
            return;
        }
        // Multi-chunk: commit the streaming path, then forward chunk 2.
        if first_tx
            .send(Ok(FirstReply::Streaming(Bytes::from(head))))
            .is_err()
        {
            return; // client disconnected
        }
        if tx
            .blocking_send(Ok(Bytes::from(ser.chunk(&vars, &rows2))))
            .is_err()
        {
            return; // client disconnected
        }
        loop {
            // One atomic load per chunk: catches a deadline that passed
            // while an operator pipeline (rather than a WCOJ scan, which
            // polls the token itself) was producing rows.
            if cancel.is_cancelled() {
                bump_exec_error();
                let _ = tx.blocking_send(Err(SparqlError::QueryTimeout));
                return;
            }
            match stream.next_chunk() {
                Ok(Some(rows)) => {
                    emitted += rows.len() as u64;
                    if emitted > max_rows {
                        // Headers are committed, so the cap surfaces the
                        // same way a mid-stream executor error does: the
                        // body aborts without its terminator rather than
                        // ending short and looking complete.
                        bump_exec_error();
                        let _ = tx.blocking_send(Err(SparqlError::ResultRowLimit(max_rows)));
                        return;
                    }
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
                    let _ = tx.blocking_send(Err(classify(e, &cancel)));
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
        // Errors before any byte was emitted are still a clean status —
        // parity with the materialized path's error handling.
        Ok(Err(e)) => (error_status(&e), e.to_string()).into_response(),
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
    cfg: &SparqlConfig,
    permit: QueryPermit,
    settings: &QuerySettings,
) -> axum::response::Response {
    let (cancel, running) = arm_timeout(settings.query_timeout.0);
    let store = Arc::clone(&state.store);
    let (query, cfg) = (q.to_string(), *cfg);

    // On the blocking pool, not this worker: like `/update`, this path takes
    // a store lock and then runs the whole query, so leaving it on a runtime
    // worker would park that worker for the query's full duration — and the
    // `query_timeout` timer, which is an async task, could never fire.
    // The read guard is scoped to the execution only; results are
    // materialised into `ans`, so the serialization below holds no lock and
    // never blocks a concurrent writer.
    let ans = tokio::task::spawn_blocking(move || {
        // HDB-118: the permit is held until this closure returns, i.e. for
        // the whole execution. Serialization below runs unpermitted, as it
        // touches no store.
        let _permit = permit;
        let _running = running;
        let _cancel_scope = crate::exec::cancel::scope(cancel.clone());
        let store = store.read();
        execute_query_with(&query, &*store, &cfg).map_err(|e| classify(e, &cancel))
    })
    .await
    .expect("query task panicked");
    let ans = match ans {
        Ok(a) => a,
        Err(e) => return (error_status(&e), e.to_string()).into_response(),
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
