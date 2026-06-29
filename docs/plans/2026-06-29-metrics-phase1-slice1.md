# Metrics Phase 1 — Slice 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an end-to-end metrics vertical slice — a new `horndb-metrics` crate with a global registry and typed labels, a `GET /metrics` scrape endpoint, and live instrumentation of the SPARQL HTTP layer, the closure backend, and storage sizes.

**Architecture:** `prometheus-client` with `#[derive(EncodeLabelSet)]` typed labels. A new foundational crate `horndb-metrics` owns the only `prometheus-client` dependency, a process-global `OnceLock<MetricsState>` holding the `Registry` (behind a `Mutex` for scrape/late-registration) and the typed `Family` handles. Hot-path code calls `horndb_metrics::metrics().<subsystem>.<metric>...`. Expensive sizes are read at scrape time via a `Collector` registered by the server with a `Weak` ref to the live store.

**Tech Stack:** Rust 1.90, `prometheus-client` 0.23, axum 0.8, tokio, the existing `horndb-storage` / `horndb-closure` / `horndb-sparql` crates.

**Reference spec:** `docs/specs/2026-06-29-metrics-design.md`

---

## File Structure

- `crates/metrics/Cargo.toml` — new crate manifest (`horndb-metrics`).
- `crates/metrics/src/lib.rs` — public surface: `metrics()`, `MetricsState`, `encode_metrics()`, `register_collector()`.
- `crates/metrics/src/labels.rs` — typed label sets/values (`Endpoint`, `Method`, `QueryKind`, `Stage`, `MemTier`).
- `crates/metrics/src/sparql.rs` — `SparqlMetrics` struct (counters + histograms).
- `crates/metrics/src/closure.rs` — `ClosureSink` (histograms/observers fed from `ClosureMetrics`).
- `crates/metrics/src/storage.rs` — `StorageMetrics` (load counters) + `StorageCollector` (scrape-time gauges).
- `Cargo.toml` (root) — add `horndb-metrics` to `[workspace.dependencies]`, members, default-members; add `prometheus-client`.
- `crates/sparql/Cargo.toml` — depend on `horndb-metrics`.
- `crates/sparql/src/server/mod.rs` — add `/metrics` route + a request-instrumentation middleware.
- `crates/sparql/src/server/metrics_route.rs` — the `/metrics` handler.
- `crates/sparql/src/api.rs` — per-stage timing in `execute_query*` / `execute_update*`.
- `crates/sparql/src/bin/serve.rs` — register the storage scrape-time collector at startup.
- `crates/closure/Cargo.toml` + closure call site — feed `ClosureMetrics` into the sink.
- `crates/storage/src/...` — ensure a cheap stats snapshot is reachable for the collector.

---

## Task 1: Create the `horndb-metrics` crate skeleton + global registry

**Files:**
- Create: `crates/metrics/Cargo.toml`
- Create: `crates/metrics/src/lib.rs`
- Modify: `Cargo.toml` (root) — `[workspace.dependencies]`, `members`, `default-members`
- Test: `crates/metrics/src/lib.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Add the crate to the workspace**

In root `Cargo.toml`, add `"crates/metrics",` to both `members` and `default-members`, and under `[workspace.dependencies]`:

```toml
horndb-metrics = { path = "crates/metrics" }
prometheus-client = "0.23"
```

- [ ] **Step 2: Write `crates/metrics/Cargo.toml`**

```toml
[package]
name = "horndb-metrics"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
prometheus-client.workspace = true
```

- [ ] **Step 3: Write the failing test**

In `crates/metrics/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_contains_registered_metric() {
        // A fresh local state so the test does not depend on global init order.
        let state = MetricsState::new();
        state.sparql.requests
            .get_or_create(&crate::labels::RequestLabels {
                endpoint: crate::labels::Endpoint::Query,
                method: crate::labels::Method::Get,
                status: 200,
            })
            .inc();
        let out = state.encode();
        assert!(out.contains("horndb_sparql_requests_total"));
        assert!(out.contains("endpoint=\"query\""));
    }
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p horndb-metrics`
Expected: FAIL — `MetricsState`, `labels`, etc. do not exist yet.

- [ ] **Step 5: Implement the registry skeleton**

In `crates/metrics/src/lib.rs`:

```rust
//! HornDB metrics: prometheus-client registry with typed labels.
//!
//! A process-global `MetricsState` (init-once via `OnceLock`) owns the
//! `Registry` and all typed metric handles. Hot-path code reaches a handle
//! through `metrics()`; expensive sizes are read at scrape time via collectors
//! registered with `register_collector`.

pub mod labels;
pub mod sparql;
pub mod closure;
pub mod storage;

use std::sync::{Mutex, OnceLock};
use prometheus_client::collector::Collector;
use prometheus_client::registry::Registry;

pub struct MetricsState {
    /// Registry behind a Mutex: registration (startup + late collectors) needs
    /// `&mut`, and scrape encoding takes `&`. Scrapes are infrequent, so the
    /// lock is uncontended on the hot path (handles are cloned out, not locked).
    registry: Mutex<Registry>,
    pub sparql: sparql::SparqlMetrics,
    pub closure: closure::ClosureSink,
    pub storage: storage::StorageMetrics,
}

impl MetricsState {
    pub fn new() -> Self {
        let mut registry = Registry::with_prefix("horndb");
        let sparql = sparql::SparqlMetrics::register(&mut registry);
        let closure = closure::ClosureSink::register(&mut registry);
        let storage = storage::StorageMetrics::register(&mut registry);
        Self { registry: Mutex::new(registry), sparql, closure, storage }
    }

    /// OpenMetrics text for a scrape.
    pub fn encode(&self) -> String {
        let mut buf = String::new();
        let reg = self.registry.lock().expect("metrics registry poisoned");
        prometheus_client::encoding::text::encode(&mut buf, &reg)
            .expect("encode into String is infallible");
        buf
    }

    /// Register a scrape-time collector (e.g. storage size gauges backed by a
    /// `Weak` ref to the live store). Call once at server startup.
    pub fn register_collector(&self, c: Box<dyn Collector>) {
        self.registry.lock().expect("metrics registry poisoned").register_collector(c);
    }
}

impl Default for MetricsState {
    fn default() -> Self { Self::new() }
}

static METRICS: OnceLock<MetricsState> = OnceLock::new();

/// The process-global metrics handles. Cheap; safe to call on any path.
pub fn metrics() -> &'static MetricsState {
    METRICS.get_or_init(MetricsState::new)
}

/// Encode the global registry for the `/metrics` endpoint.
pub fn encode_metrics() -> String {
    metrics().encode()
}

/// Register a collector against the global registry.
pub fn register_collector(c: Box<dyn Collector>) {
    metrics().register_collector(c);
}
```

> NOTE: `Registry::with_prefix("horndb")` makes every series start `horndb_`; the
> per-metric names below therefore OMIT the leading `horndb_`. If the installed
> `prometheus-client` version names the method differently, adapt — the test in
> Step 3 asserts the final `horndb_sparql_requests_total` string.

- [ ] **Step 6: Run the test (will still fail until Tasks 2 modules exist)**

The `labels`, `sparql`, `closure`, `storage` modules are created in Task 2. Proceed to Task 2, then return and run `cargo test -p horndb-metrics` — expected PASS.

- [ ] **Step 7: Commit (after Task 2 compiles)**

```bash
git add Cargo.toml crates/metrics
git commit -m "feat(metrics): horndb-metrics crate skeleton + global OnceLock registry"
```

---

## Task 2: Typed labels and per-subsystem metric structs

**Files:**
- Create: `crates/metrics/src/labels.rs`
- Create: `crates/metrics/src/sparql.rs`
- Create: `crates/metrics/src/closure.rs`
- Create: `crates/metrics/src/storage.rs`

- [ ] **Step 1: Write `labels.rs`**

```rust
//! Typed label sets and values. No strings at call sites.
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue};

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Endpoint { Query, Update, Metrics }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Method { Get, Post }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum QueryKind { Select, Ask, Construct, Describe, Update }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Stage { Parse, Translate, Plan, Exec }

/// Memory tier — most variants are aspirational (HBM/CXL not yet built) but the
/// label exists now so adding real tiering later is a value change, not a schema
/// change. See spec §7.3.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum MemTier { Dram, Hbm, Cxl, Unknown }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    pub endpoint: Endpoint,
    pub method: Method,
    pub status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct EndpointLabel { pub endpoint: Endpoint }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct QueryKindLabel { pub kind: QueryKind }

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StageLabel { pub stage: Stage }
```

- [ ] **Step 2: Write `sparql.rs`**

```rust
//! SPARQL HTTP + pipeline metrics.
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::{EndpointLabel, QueryKindLabel, RequestLabels, StageLabel};

#[derive(Clone)]
pub struct SparqlMetrics {
    pub requests: Family<RequestLabels, Counter>,
    pub request_duration_seconds: Family<EndpointLabel, Histogram>,
    pub response_bytes: Family<EndpointLabel, Counter>,
    pub query_total: Family<QueryKindLabel, Counter>,
    pub query_errors: Family<StageLabel, Counter>,
    pub stage_duration_seconds: Family<StageLabel, Histogram>,
}

fn latency_hist() -> Histogram {
    // 100µs .. ~10s, 12 buckets — request/stage latencies.
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl SparqlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let requests = Family::<RequestLabels, Counter>::default();
        let request_duration_seconds =
            Family::<EndpointLabel, Histogram>::new_with_constructor(latency_hist);
        let response_bytes = Family::<EndpointLabel, Counter>::default();
        let query_total = Family::<QueryKindLabel, Counter>::default();
        let query_errors = Family::<StageLabel, Counter>::default();
        let stage_duration_seconds =
            Family::<StageLabel, Histogram>::new_with_constructor(latency_hist);

        reg.register("sparql_requests", "Total SPARQL HTTP requests", requests.clone());
        reg.register("sparql_request_duration_seconds", "SPARQL request latency", request_duration_seconds.clone());
        reg.register("sparql_response_bytes", "SPARQL response bytes written", response_bytes.clone());
        reg.register("sparql_query", "SPARQL operations by kind", query_total.clone());
        reg.register("sparql_query_errors", "SPARQL pipeline errors by stage", query_errors.clone());
        reg.register("sparql_stage_duration_seconds", "SPARQL pipeline stage latency", stage_duration_seconds.clone());

        Self { requests, request_duration_seconds, response_bytes, query_total, query_errors, stage_duration_seconds }
    }
}
```

- [ ] **Step 3: Write `closure.rs`**

```rust
//! Closure backend metrics. Fed from `horndb_closure::ClosureMetrics` after each
//! transitive-closure call (per-call, not per-iteration — see spec §5.3).
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct ClosureSink {
    pub mxm_seconds: Histogram,
    pub total_seconds: Histogram,
    pub iterations_to_fixpoint: Histogram,
    pub output_nnz: Histogram,
}

impl ClosureSink {
    pub fn register(reg: &mut Registry) -> Self {
        let mxm_seconds = Histogram::new(exponential_buckets(1e-4, 3.0, 12));
        let total_seconds = Histogram::new(exponential_buckets(1e-4, 3.0, 12));
        let iterations_to_fixpoint = Histogram::new(exponential_buckets(1.0, 2.0, 10));
        let output_nnz = Histogram::new(exponential_buckets(10.0, 10.0, 9));
        reg.register("closure_mxm_seconds", "Time in GrB_mxm per closure call", mxm_seconds.clone());
        reg.register("closure_total_seconds", "Total closure wall time per call", total_seconds.clone());
        reg.register("closure_iterations_to_fixpoint", "Iterations to closure fixpoint", iterations_to_fixpoint.clone());
        reg.register("closure_output_nnz", "Closure output non-zeros per call", output_nnz.clone());
        Self { mxm_seconds, total_seconds, iterations_to_fixpoint, output_nnz }
    }

    /// Record one closure call. `mxm`/`total` in seconds.
    pub fn observe(&self, mxm: f64, total: f64, iterations: u64, output_nnz: u64) {
        self.mxm_seconds.observe(mxm);
        self.total_seconds.observe(total);
        self.iterations_to_fixpoint.observe(iterations as f64);
        self.output_nnz.observe(output_nnz as f64);
    }
}
```

- [ ] **Step 4: Write `storage.rs` (counters now; the scrape-time collector is added in Task 6)**

```rust
//! Storage metrics. Load-path counters live here; size gauges are produced by a
//! scrape-time collector registered by the server (Task 6).
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct StorageMetrics {
    pub load_duration_seconds: Histogram,
    pub load_bytes: Counter,
}

impl StorageMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let load_duration_seconds = Histogram::new(exponential_buckets(1e-3, 3.0, 12));
        let load_bytes = Counter::default();
        reg.register("storage_load_duration_seconds", "RDF load duration", load_duration_seconds.clone());
        reg.register("storage_load_bytes", "Bytes read during RDF load", load_bytes.clone());
        Self { load_duration_seconds, load_bytes }
    }
}
```

- [ ] **Step 5: Run Task 1's test**

Run: `cargo test -p horndb-metrics`
Expected: PASS (`encode_contains_registered_metric`).

- [ ] **Step 6: Commit**

```bash
git add crates/metrics
git commit -m "feat(metrics): typed labels + sparql/closure/storage metric structs"
```

---

## Task 3: `/metrics` endpoint + request-instrumentation middleware

**Files:**
- Modify: `crates/sparql/Cargo.toml` — add `horndb-metrics.workspace = true`
- Create: `crates/sparql/src/server/metrics_route.rs`
- Modify: `crates/sparql/src/server/mod.rs` — add route + middleware
- Test: `crates/sparql/tests/metrics_endpoint.rs`

- [ ] **Step 1: Add the dependency**

In `crates/sparql/Cargo.toml` `[dependencies]`: `horndb-metrics.workspace = true`.

- [ ] **Step 2: Write the failing integration test**

`crates/sparql/tests/metrics_endpoint.rs`:

```rust
#![cfg(feature = "server")]
use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::server::{build_router, AppState};
use std::sync::{Arc, RwLock};
use tower::ServiceExt; // oneshot

#[tokio::test]
async fn metrics_endpoint_exposes_request_counter() {
    let state = AppState::<MemStore> { store: Arc::new(RwLock::new(MemStore::default())) };
    let app = build_router(state);

    // Issue one query so the counter is non-zero.
    let q = "SELECT * WHERE { ?s ?p ?o } LIMIT 1";
    let _ = app.clone().oneshot(
        Request::builder().method("GET")
            .uri(format!("/query?query={}", urlencoding::encode(q)))
            .body(Body::empty()).unwrap()
    ).await.unwrap();

    let resp = app.oneshot(
        Request::builder().uri("/metrics").body(Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("horndb_sparql_requests_total"), "got:\n{text}");
}
```

> If `MemStore` has no `Default`, construct it however the existing server tests do
> (grep `tests/` for `AppState::<MemStore>`); reuse that exact constructor.
> Add `urlencoding` as a dev-dependency only if not already present, or build the
> query string with `axum`'s test helpers used elsewhere.

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p horndb-sparql --features server metrics_endpoint`
Expected: FAIL — no `/metrics` route (404).

- [ ] **Step 4: Write the `/metrics` handler**

`crates/sparql/src/server/metrics_route.rs`:

```rust
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;

/// `GET /metrics` — OpenMetrics text for Prometheus scrape.
pub async fn handle_metrics() -> impl IntoResponse {
    let body = horndb_metrics::encode_metrics();
    ([(CONTENT_TYPE, "application/openmetrics-text; version=1.0.0; charset=utf-8")], body)
}
```

- [ ] **Step 5: Add the route and the instrumentation middleware**

In `crates/sparql/src/server/mod.rs`, add `pub mod metrics_route;` and extend `build_router`:

```rust
use axum::middleware;

pub fn build_router<B: FullBackend + Send + Sync + 'static>(state: AppState<B>) -> Router {
    Router::new()
        .route(
            "/query",
            get(query::handle_query_get::<B>).post(query::handle_query_post::<B>),
        )
        .route("/update", post(update::handle_update::<B>))
        .route("/metrics", get(metrics_route::handle_metrics))
        .layer(middleware::from_fn(record_request))
        .with_state(state)
}
```

Add the middleware fn in the same file:

```rust
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use horndb_metrics::labels::{Endpoint, Method, EndpointLabel, RequestLabels};
use std::time::Instant;

async fn record_request(req: Request, next: Next) -> Response {
    let endpoint = match req.uri().path() {
        "/query" => Some(Endpoint::Query),
        "/update" => Some(Endpoint::Update),
        "/metrics" => Some(Endpoint::Metrics),
        _ => None,
    };
    let method = if req.method() == axum::http::Method::GET { Method::Get } else { Method::Post };
    let start = Instant::now();
    let resp = next.run(req).await;
    if let Some(endpoint) = endpoint {
        let m = horndb_metrics::metrics();
        m.sparql.request_duration_seconds
            .get_or_create(&EndpointLabel { endpoint: endpoint.clone() })
            .observe(start.elapsed().as_secs_f64());
        m.sparql.requests
            .get_or_create(&RequestLabels { endpoint, method, status: resp.status().as_u16() })
            .inc();
    }
    resp
}
```

- [ ] **Step 6: Run the test**

Run: `cargo test -p horndb-sparql --features server metrics_endpoint`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/sparql
git commit -m "feat(metrics): /metrics endpoint + request instrumentation middleware"
```

---

## Task 4: Per-stage timing + query-kind counters in the pipeline

**Files:**
- Modify: `crates/sparql/src/api.rs` (around `execute_query_with` ~line 41, `execute_update_with` ~line 130)
- Test: `crates/sparql/tests/metrics_pipeline.rs`

- [ ] **Step 1: Write the failing test**

`crates/sparql/tests/metrics_pipeline.rs`:

```rust
#![cfg(feature = "server")]
use horndb_sparql::exec::mem::MemStore;
// Reuse whatever public execute entrypoint the existing api tests use.

#[test]
fn query_kind_and_stage_metrics_recorded() {
    let store = MemStore::default();
    let q = "SELECT * WHERE { ?s ?p ?o } LIMIT 1";
    let _ = horndb_sparql::execute_query(&store, q).expect("query ok");

    let text = horndb_metrics::encode_metrics();
    assert!(text.contains("horndb_sparql_query_total"));
    assert!(text.contains("kind=\"select\""));
    assert!(text.contains("horndb_sparql_stage_duration_seconds"));
}
```

> Match the real signature of `execute_query` (Task scan: `api.rs:35`). If it needs a
> `SparqlConfig`, use `execute_query_with` with the default config, mirroring existing tests.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-sparql --features server metrics_pipeline`
Expected: FAIL — no `horndb_sparql_query_total`.

- [ ] **Step 3: Instrument the pipeline**

In `crates/sparql/src/api.rs`, wrap each stage with `Instant::now()` and record. Sketch (adapt to the real control flow):

```rust
use std::time::Instant;
use horndb_metrics::labels::{QueryKind, QueryKindLabel, Stage, StageLabel};

// inside execute_query_with, after parsing succeeds:
let m = horndb_metrics::metrics();
// parse
let t = Instant::now();
let ast = match parse_query(text) {
    Ok(a) => a,
    Err(e) => { m.sparql.query_errors.get_or_create(&StageLabel{stage: Stage::Parse}).inc(); return Err(e.into()); }
};
m.sparql.stage_duration_seconds.get_or_create(&StageLabel{stage: Stage::Parse}).observe(t.elapsed().as_secs_f64());

// classify kind from the parsed AST (Select/Ask/Construct/Describe)
let kind = classify_kind(&ast); // small helper matching on the query form
m.sparql.query_total.get_or_create(&QueryKindLabel{ kind }).inc();

// repeat the Instant pattern for translate / plan / exec, each with its Stage and
// incrementing query_errors on the matching stage on error.
```

Add a private helper:

```rust
fn classify_kind(ast: &spargebra::Query) -> horndb_metrics::labels::QueryKind {
    use horndb_metrics::labels::QueryKind::*;
    match ast {
        spargebra::Query::Select { .. } => Select,
        spargebra::Query::Ask { .. } => Ask,
        spargebra::Query::Construct { .. } => Construct,
        spargebra::Query::Describe { .. } => Describe,
    }
}
```

In `execute_update_with`, increment `query_total{kind=update}` and time an `Exec` stage.

> Keep the diff minimal and behavior-preserving: only add timing/counters around the
> existing calls; do not restructure the pipeline. If the function returns early in
> several places, record the error counter at each early-return matching the stage.

- [ ] **Step 4: Run the test**

Run: `cargo test -p horndb-sparql --features server metrics_pipeline`
Expected: PASS.

- [ ] **Step 5: Run the full sparql suite to check no regressions**

Run: `cargo test -p horndb-sparql --features server`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql
git commit -m "feat(metrics): per-stage timing + query-kind counters in SPARQL pipeline"
```

---

## Task 5: Feed `ClosureMetrics` into the closure sink

**Files:**
- Modify: `crates/closure/Cargo.toml` — add `horndb-metrics.workspace = true`
- Modify: `crates/closure/src/metrics.rs` (the `valued_transitive_closure` return path, ~line 107/193) and/or `crates/closure/src/crosswalk.rs:73-94`
- Test: `crates/closure/tests/metrics_sink.rs`

- [ ] **Step 1: Add the dependency**

In `crates/closure/Cargo.toml` `[dependencies]`: `horndb-metrics.workspace = true`.

- [ ] **Step 2: Write the failing test**

`crates/closure/tests/metrics_sink.rs`:

```rust
// Build a tiny graph, run the closure, assert the global sink saw one observation.
#[test]
fn closure_call_records_metrics() {
    // Construct the smallest input the existing closure unit tests use; grep
    // crates/closure/src for an existing `valued_transitive_closure` test and
    // mirror its setup.
    run_a_minimal_closure();
    let text = horndb_metrics::encode_metrics();
    assert!(text.contains("horndb_closure_total_seconds"));
    // a histogram with >=1 sample has a _count line > 0
    assert!(text.contains("horndb_closure_total_seconds_count"));
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p horndb-closure metrics_sink`
Expected: FAIL.

- [ ] **Step 4: Emit into the sink where `ClosureMetrics` is finalized**

At the point `valued_transitive_closure` builds its `ClosureMetrics` (before returning), add:

```rust
horndb_metrics::metrics().closure.observe(
    metrics.mxm_time.as_secs_f64(),
    metrics.total_time.as_secs_f64(),
    metrics.iterations_to_fixpoint,
    metrics.closure_nnz,
);
```

(Use the actual field names from `ClosureMetrics` in `crates/closure/src/metrics.rs:41-80`:
`mxm_time`, `total_time`, `iterations_to_fixpoint`, `closure_nnz`.)

- [ ] **Step 5: Run the test + closure suite**

Run: `cargo test -p horndb-closure`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/closure
git commit -m "feat(metrics): record ClosureMetrics into the global closure sink"
```

---

## Task 6: Storage scrape-time collector + load-path counters

**Files:**
- Modify: `crates/metrics/src/storage.rs` — add `StorageCollector` + a `StorageSnapshot` struct
- Modify: `crates/sparql/src/bin/serve.rs` — register the collector at startup; count load bytes/time
- Test: `crates/metrics/src/storage.rs` (inline test for the collector encoding)

> Slice 1 does NOT add a `horndb-metrics` dependency to `horndb-storage`: storage emits
> nothing itself here. The size gauges are read by a collector closure in `serve.rs`, and
> the load counters are recorded in `serve.rs`. The owlrl/incremental fan-out is where
> lower crates start depending on `horndb-metrics` directly.

- [ ] **Step 1: Add a scrape-time collector to `storage.rs`**

```rust
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::gauge::ConstGauge;

/// A cheap snapshot of storage sizes, read at scrape time.
#[derive(Clone, Copy, Default)]
pub struct StorageSnapshot {
    pub triples: i64,
    pub graphs: i64,
    pub predicates: i64,
    pub dictionary_terms: i64,
    pub tier_bytes_estimated: i64,
}

/// Produces gauges by calling `f` at each scrape. `f` returns `None` if the store
/// is gone (e.g. server shutting down).
pub struct StorageCollector {
    f: Box<dyn Fn() -> Option<StorageSnapshot> + Send + Sync>,
}

impl StorageCollector {
    pub fn new(f: impl Fn() -> Option<StorageSnapshot> + Send + Sync + 'static) -> Self {
        Self { f: Box::new(f) }
    }
}

impl std::fmt::Debug for StorageCollector {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.write_str("StorageCollector")
    }
}

impl Collector for StorageCollector {
    fn encode(&self, mut enc: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let snap = (self.f)().unwrap_or_default();
        for (name, help, val) in [
            ("storage_triples", "Live triples in the store", snap.triples),
            ("storage_graphs", "Distinct named graphs", snap.graphs),
            ("storage_predicates", "Distinct predicates", snap.predicates),
            ("storage_dictionary_terms", "Interned dictionary terms", snap.dictionary_terms),
            ("storage_tier_bytes_estimated", "Estimated tier bytes", snap.tier_bytes_estimated),
        ] {
            let g = ConstGauge::new(val);
            let me = enc.encode_descriptor(name, help, None, g.metric_type())?;
            g.encode(me)?;
        }
        Ok(())
    }
}
```

> The exact `Collector`/`DescriptorEncoder` API differs slightly across
> `prometheus-client` minor versions. If signatures differ, follow the version's
> `collector` module docs; the contract is: emit five gauges from one `StorageSnapshot`.

- [ ] **Step 2: Inline test for the collector**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use prometheus_client::registry::Registry;

    #[test]
    fn collector_emits_storage_gauges() {
        let mut reg = Registry::with_prefix("horndb");
        reg.register_collector(Box::new(StorageCollector::new(|| Some(StorageSnapshot {
            triples: 42, graphs: 1, predicates: 3, dictionary_terms: 99, tier_bytes_estimated: 1024,
        }))));
        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_storage_triples 42"));
        assert!(buf.contains("horndb_storage_dictionary_terms 99"));
    }
}
```

- [ ] **Step 3: Run to verify it fails, then it passes after Step 1 compiles**

Run: `cargo test -p horndb-metrics collector_emits_storage_gauges`
Expected: PASS once the collector compiles.

- [ ] **Step 4: Provide a cheap snapshot from the store**

Confirm `HornBackend` (or the underlying storage tier) exposes a cheap stats accessor
(`TierStats` is used at `crates/storage/src/store.rs:47,67,72`). If a public
`fn stats(&self) -> TierStats` (or similar) is missing, add a thin one that returns the
already-computed counts — do NOT add an O(n) scan. Map its fields into `StorageSnapshot`.

- [ ] **Step 5: Register the collector + count load in `serve.rs`**

In `crates/sparql/src/bin/serve.rs`, after building `state` (line ~97) and before serving:

```rust
let store_weak = std::sync::Arc::downgrade(&state.store);
horndb_metrics::register_collector(Box::new(
    horndb_metrics::storage::StorageCollector::new(move || {
        let arc = store_weak.upgrade()?;
        let guard = arc.read().ok()?;
        let s = guard.stats(); // the cheap accessor from Step 4
        Some(horndb_metrics::storage::StorageSnapshot {
            triples: s.triples as i64,
            graphs: s.graphs as i64,
            predicates: s.predicates as i64,
            dictionary_terms: s.dictionary_size() as i64, // or the right field
            tier_bytes_estimated: s.bytes_estimated as i64,
        })
    }),
));
```

And in `load_file` / the load loop, after timing the load, record:

```rust
horndb_metrics::metrics().storage.load_bytes.inc_by(bytes_read);
horndb_metrics::metrics().storage.load_duration_seconds.observe(elapsed_secs);
```

> `serve.rs` currently does not time loads; wrap the existing per-file load with an
> `Instant::now()`/`.elapsed()` and pass the file byte length as `bytes_read`.

- [ ] **Step 6: Build the serve binary + run metrics crate tests**

Run: `cargo build -p horndb-sparql --features server --bin serve`
Run: `cargo test -p horndb-metrics`
Expected: both succeed.

- [ ] **Step 7: Commit**

```bash
git add crates/metrics crates/storage crates/sparql
git commit -m "feat(metrics): scrape-time storage size collector + load-path counters"
```

---

## Task 7: Docs sync, lint, and final verification

**Files:**
- Modify: `docs/architecture.md` (add Observability/Metrics row)
- Modify: `TASKS.md` (add metrics epic + slice-1 done + fan-out tasks)
- Mirror to a GitHub issue per the `TASKS.md` header procedure

- [ ] **Step 1: Update `docs/architecture.md`**

Add a row/section for **Observability / Metrics**: Status **implemented** for Slice 1
(prometheus-client, `/metrics`, sparql+closure+storage), **planned** for the owlrl /
incremental / ml / wcoj fan-out. Link the spec.

- [ ] **Step 2: Update `TASKS.md`**

Add a metrics epic with Slice 1 checked off and the fan-out items listed (owlrl,
incremental, ml, wcoj, traces/logs as a future phase), at the priority the project lead
sets. Mirror to a GitHub issue per the header procedure.

- [ ] **Step 3: Workspace lint + build (what CI runs)**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo nextest run -p horndb-metrics -p horndb-closure -p horndb-storage`
Run: `cargo nextest run -p horndb-sparql --features server`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md TASKS.md
git commit -m "docs(metrics): record Phase-1 Slice-1 metrics in architecture + TASKS"
```

---

## Notes for the executor

- `prometheus-client` minor-version API drift is the main risk: `Registry::with_prefix`,
  `new_with_constructor`, and the `Collector`/`DescriptorEncoder` signatures are the
  spots to verify against the actually-resolved version. The tests assert the *encoded
  output strings*, which are stable across versions — let them drive the exact API.
- Do NOT add per-tuple/per-seek timing (spec §5.3). wcoj is out of this slice entirely.
- Keep `prometheus-client` confined to `horndb-metrics`; other crates depend only on
  `horndb-metrics`.
- Tests use the global registry via `encode_metrics()`. Because tests in one binary share
  the process-global state, assert on *presence/`_count`* of series rather than exact
  counter values to avoid cross-test coupling.
