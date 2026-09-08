//! SPEC-31 AC5 — the peak-memory histogram is observed exactly once per
//! query, on every exit path: a clean streaming finish, an over-budget
//! refusal, and a clean finish for a blocking operator that fits.
//!
//! Its own test binary: `horndb_metrics::metrics()` is process-global.
//! `cargo nextest` gives each test its own process, but `cargo test` runs a
//! binary's tests as parallel threads in one process, where a count-delta
//! assertion would flake against other tests observing the same metric
//! (`tests/graph_store_materialized.rs` isolates process-global state the
//! same way).
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_config::Limits;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use horndb_sparql::server::{build_router, AppState};
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn router_with_rows(n: u64) -> axum::Router {
    let mut s = MemStore::default();
    for i in 0..n {
        s.insert_triple(
            iri(&format!("http://ex/s{i}")),
            iri("http://ex/p"),
            iri(&format!("http://ex/o{i}")),
        );
    }
    build_router(AppState {
        store: Arc::new(RwLock::new(s)),
        config: horndb_config::ConfigHandle::from_limits(Limits::default()),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    })
}

async fn get(app: axum::Router, uri: &str) -> StatusCode {
    let req = Request::builder()
        .uri(uri)
        .header("accept", "text/csv")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    // Drain the body: the response is streamed, and the executor's `Scope`
    // (and the metric observation in its `Drop`) does not finish until the
    // body is fully read.
    let _ = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024).await;
    status
}

/// Parse a prometheus-client text-format counter/histogram-count value for a
/// line that starts with `metric_name`. Copied from the private
/// `parse_counter` in `tests/server_http.rs` (not importable across test
/// binaries).
fn parse_counter(output: &str, metric_name: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        if line.starts_with(metric_name) {
            line.split_whitespace().last()?.parse::<u64>().ok()
        } else {
            None
        }
    })
}

/// Poll `encode_metrics()` until `metric_name`'s value reaches `target`, or
/// fail after `deadline`.
///
/// The observation this test checks for happens in `budget::Scope::drop`,
/// which runs on the server's blocking-pool thread — a thread that can
/// finish *after* the HTTP response has already been handed back to the
/// caller (this test). Reading the metric right after `get()` returns is
/// therefore racy; polling with a bounded deadline is not the same as a bare
/// sleep-and-hope, it waits only as long as the drop actually takes and
/// still fails deterministically if it never happens.
async fn poll_for_count(metric_name: &str, target: u64, deadline: Duration) -> u64 {
    let start = Instant::now();
    loop {
        let value = parse_counter(&horndb_metrics::encode_metrics(), metric_name).unwrap_or(0);
        if value >= target {
            return value;
        }
        if start.elapsed() > deadline {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn peak_memory_metric_observed_exactly_once_per_query_on_every_exit_path() {
    let app = router_with_rows(5_000);
    let order_by_q =
        "/query?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20ORDER%20BY%20%3Fs";
    let select_all_q =
        "/query?query=SELECT%20%3Fs%20%3Fp%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D";

    let metrics = horndb_metrics::encode_metrics();
    let peak_before =
        parse_counter(&metrics, "horndb_sparql_query_memory_peak_bytes_count").unwrap_or(0);
    let over_budget_before =
        parse_counter(&metrics, "horndb_sparql_queries_over_budget_total").unwrap_or(0);

    // (a) Streaming SELECT_ALL: no blocking operator, so it charges nothing
    // (peak 0) but the scope still must observe on the way out.
    let status = get(app.clone(), select_all_q).await;
    assert_eq!(status, StatusCode::OK, "streaming query");

    // (b) ORDER BY has to buffer its whole input, so a 1-byte ceiling refuses
    // it — the over-budget exit path.
    let status = get(app.clone(), &format!("{order_by_q}&max_query_memory=1")).await;
    assert_eq!(
        status,
        StatusCode::INSUFFICIENT_STORAGE,
        "over-budget ORDER BY"
    );

    // (c) Same ORDER BY at the default budget: a clean blocking-operator
    // finish.
    let status = get(app, order_by_q).await;
    assert_eq!(status, StatusCode::OK, "ORDER BY within budget");

    let deadline = Duration::from_secs(2);
    poll_for_count(
        "horndb_sparql_query_memory_peak_bytes_count",
        peak_before + 3,
        deadline,
    )
    .await;
    poll_for_count(
        "horndb_sparql_queries_over_budget_total",
        over_budget_before + 1,
        deadline,
    )
    .await;

    // Settle window: the poll above returns the instant the count first
    // reaches its target, which rules out a missing observation but not an
    // extra one landing a beat later — "exactly once" needs both. Every
    // request's body is already fully drained by this point, so nothing
    // should still be writing to the metric; a short quiet period followed
    // by one more read catches a duplicate observation the poll's early
    // return would otherwise miss.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let metrics = horndb_metrics::encode_metrics();
    let peak_after =
        parse_counter(&metrics, "horndb_sparql_query_memory_peak_bytes_count").unwrap_or(0);
    let over_budget_after =
        parse_counter(&metrics, "horndb_sparql_queries_over_budget_total").unwrap_or(0);

    assert_eq!(
        peak_after,
        peak_before + 3,
        "the peak-memory histogram must be observed exactly once per query \
         across all three exit paths (clean streaming finish, over-budget \
         refusal, clean blocking finish)"
    );
    assert_eq!(
        over_budget_after,
        over_budget_before + 1,
        "exactly one of the three queries went over budget"
    );
}
