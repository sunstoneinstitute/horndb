//! Embedded HTTP server exposing SPARQL 1.1 Protocol endpoints.
//!
//! `/query` and `/update` are the SPARQL 1.1 Protocol surface; `/graphs` is
//! the Graph Store Protocol (SPEC-28 S5, `graph_store`); `/metrics` is the
//! Prometheus scrape target; `/healthz` and `/readyz` are the Kubernetes
//! liveness/readiness probes (HDB-124).

mod counting_body;
pub mod graph_store;
mod health;
pub mod metrics_route;
pub mod query;
mod request_id;
mod stream_body;
pub mod update;

use crate::exec::mem::MemStore;
use crate::exec::FullBackend;
use axum::extract::{DefaultBodyLimit, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use counting_body::{CountingBody, Direction};
use horndb_metrics::labels::{Endpoint, EndpointLabel, Method, RequestLabels};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Fallback slot count when the host core count is unavailable. Mirrors
/// `horndb_config::Limits`'s own fallback.
const DEFAULT_MAX_CONCURRENT_QUERIES: usize = 8;

/// Admission control for `/query`, plus the request-body cap for `/query`
/// and `/update` (HDB-118). Built from `[server.limits]` in `serve.rs`; the
/// `Default` restates that crate's defaults so a library caller or test that
/// assembles an [`AppState`] by hand is bounded too.
///
/// Without this, every request went straight to `spawn_blocking` (default
/// cap: 512 threads), each `GRAPH`-scoped query building its own snapshot —
/// so a burst had no ceiling on memory or CPU.
#[derive(Clone)]
pub struct Limits {
    slots: Arc<Semaphore>,
    /// How long a request waits for a slot before it is shed with 503.
    pub queue_timeout: Duration,
    /// Cap on the `/query` / `/update` request body, in bytes.
    pub max_request_body: usize,
}

impl Limits {
    /// `max_concurrent_queries` is clamped to at least 1: a zero-permit
    /// semaphore would wedge the endpoint. `serve.rs` rejects `0` at startup
    /// so a config typo is loud rather than silently clamped.
    pub fn new(
        max_concurrent_queries: usize,
        queue_timeout: Duration,
        max_request_body: usize,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(max_concurrent_queries.max(1))),
            queue_timeout,
            max_request_body,
        }
    }

    /// Wait for an execution slot, up to `queue_timeout`. `None` means the
    /// caller must shed the request (503 + `Retry-After`).
    pub(crate) async fn acquire(&self) -> Option<QueryPermit> {
        let slots = Arc::clone(&self.slots);
        match tokio::time::timeout(self.queue_timeout, slots.acquire_owned()).await {
            Ok(Ok(permit)) => {
                horndb_metrics::metrics().sparql.queries_in_flight.inc();
                Some(QueryPermit { _permit: permit })
            }
            // Timed out, or the semaphore was closed (shutdown). Either way
            // the request cannot run now.
            _ => {
                horndb_metrics::metrics().sparql.queries_rejected.inc();
                None
            }
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(DEFAULT_MAX_CONCURRENT_QUERIES),
            Duration::from_secs(5),
            4 * 1024 * 1024,
        )
    }
}

/// One held execution slot. Dropping it frees the slot and decrements the
/// in-flight gauge, so every exit path — clean finish, client disconnect,
/// error, panic-unwind — releases exactly once.
pub(crate) struct QueryPermit {
    _permit: OwnedSemaphorePermit,
}

impl Drop for QueryPermit {
    fn drop(&mut self) {
        horndb_metrics::metrics().sparql.queries_in_flight.dec();
    }
}

/// Response header stamped on every response while the served closure is known
/// to be OWL 2 RL inconsistent (`[reasoning].on_inconsistency = "serve-with-flag"`).
pub const INCONSISTENT_HEADER: &str = "x-horndb-inconsistent";

/// Whether to stamp [`INCONSISTENT_HEADER`]. Process-global rather than a field
/// on [`AppState`]: it is a startup fact about the one closure this process
/// serves, and a static keeps the flag out of every generic handler signature
/// (same shape as the process-global metrics registry).
static SERVE_WITH_INCONSISTENT_FLAG: AtomicBool = AtomicBool::new(false);

/// Turn the [`INCONSISTENT_HEADER`] stamp on. Called once at startup by
/// `serve`; there is no way back — the closure does not become consistent
/// while the process runs.
pub fn flag_inconsistent() {
    SERVE_WITH_INCONSISTENT_FLAG.store(true, Ordering::Relaxed);
}

/// Whether this process serves a `--materialize` store. Process-global for
/// the same reason as [`SERVE_WITH_INCONSISTENT_FLAG`]: it is a startup fact
/// about the one closure this process serves.
///
/// SPEC-28 S5 reads it to refuse Graph Store Protocol writes to `?default`:
/// `load_with_reasoning` puts asserted and inferred triples into the default
/// graph indistinguishably, so a whole-graph write there would delete
/// inferences the client never sent.
static SERVE_MATERIALIZED: AtomicBool = AtomicBool::new(false);

/// Record that this process's store was built with `--materialize`. Called
/// once at startup by `serve`; like [`flag_inconsistent`], one-way.
pub fn flag_materialized() {
    SERVE_MATERIALIZED.store(true, Ordering::Relaxed);
}

pub(crate) fn is_materialized() -> bool {
    SERVE_MATERIALIZED.load(Ordering::Relaxed)
}

/// Shared state, generic over the storage backend. Defaults to the
/// Stage-1 `MemStore` so existing constructors keep compiling; the
/// `serve` binary instantiates `AppState<HornBackend>`.
///
/// The store is wrapped in an `RwLock` so concurrent SPARQL queries
/// take the read lock and run in parallel, while SPARQL Update takes
/// the write lock. SPEC-02 will replace this with MVCC.
///
/// `parking_lot::RwLock`, not `std::sync::RwLock`: a panic while a
/// std guard is held poisons the lock, so every later request panics too
/// (HDB-114). parking_lot never poisons — a panicking handler loses only
/// its own request.
///
/// `config` is the live `ServerConfig` (SPEC-26 S3, `horndb_config::ConfigHandle`).
/// Each request takes a cheap snapshot of it, so an operator edit that the
/// reload watcher publishes is visible to the very next request without
/// restarting anything. Its `[server.limits]` table (SPEC-26 S2) holds the
/// server-scoped *defaults* for every query-overridable setting; the request
/// layers its URL/form overrides on top to get its own `QuerySettings`
/// (SPEC-26 S4, `query::resolve_settings`), and the two of those the SPARQL
/// pipeline itself needs (`rdf12`, `default_graph`) become that request's
/// [`crate::SparqlConfig`]. Not to be confused with `admission` below: that is
/// this module's [`Limits`] (the concurrency gate built from three of the same
/// `[server.limits]` keys), which is restart-only — the semaphore is sized once
/// at startup.
///
/// `ready` backs `GET /readyz` (HDB-124): `false` until the `serve` binary's
/// startup data load (and any `--materialize` pass) finishes, then flipped
/// once via `Ordering::Release` in `bin/serve.rs`. A caller that only builds
/// a router in-process (every test in this crate) should set it `true` up
/// front — the data is already loaded by construction.
///
/// Note: `#[derive(Clone)]` is intentionally avoided here — it would
/// wrongly require `B: Clone`. The manual impl clones only the `Arc`s and
/// the small `limits` table.
pub struct AppState<B: FullBackend + Send + Sync + 'static = MemStore> {
    pub store: Arc<RwLock<B>>,
    pub config: horndb_config::ConfigHandle,
    pub ready: Arc<AtomicBool>,
    /// Admission control + request-body cap (HDB-118).
    pub admission: Limits,
}

impl<B: FullBackend + Send + Sync + 'static> AppState<B> {
    /// `Some(503)` while the startup data load is still running (HDB-144).
    ///
    /// `serve` binds the listener before the load finishes, so the process is
    /// reachable — and answers — during a long import. `/readyz` reports that
    /// correctly, but `/query` and `/update` did not: they served whatever
    /// fraction of the corpus had landed. A benchmark driver that waits on
    /// `/query` therefore measured a partly-loaded store and reported it as a
    /// result, which is how an SPB run came back with "no Creative Works were
    /// found". A data endpoint must shed the request instead of answering it
    /// from an incomplete store.
    pub(crate) fn shed_while_loading(&self) -> Option<Response> {
        use axum::response::IntoResponse;
        if self.ready.load(Ordering::Acquire) {
            return None;
        }
        Some(
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                [("retry-after", "1")],
                "store is still loading\n",
            )
                .into_response(),
        )
    }
}

impl<B: FullBackend + Send + Sync + 'static> Clone for AppState<B> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            config: self.config.clone(),
            ready: Arc::clone(&self.ready),
            admission: self.admission.clone(),
        }
    }
}

/// Build the axum router. Callers attach it to a `tokio::net::TcpListener`.
pub fn build_router<B: FullBackend + Send + Sync + 'static>(state: AppState<B>) -> Router {
    let body_limit = state.admission.max_request_body;
    Router::new()
        .route(
            "/query",
            get(query::handle_query_get::<B>).post(query::handle_query_post::<B>),
        )
        .route("/update", post(update::handle_update::<B>))
        // SPEC-28 S5. Same body cap as `/update` (413 comes from the
        // `DefaultBodyLimit` layer below).
        .route(
            "/graphs",
            get(graph_store::handle_get::<B>)
                .put(graph_store::handle_put::<B>)
                .post(graph_store::handle_post::<B>)
                .delete(graph_store::handle_delete::<B>),
        )
        // Applies to the routes added above only — `/metrics` is registered
        // after it and keeps axum's default. `LOAD` reads files, not request
        // bodies, so bulk ingest is unaffected.
        .layer(DefaultBodyLimit::max(body_limit))
        .route("/metrics", get(metrics_route::handle_metrics))
        .route("/healthz", get(health::handle_healthz))
        .route("/readyz", get(health::handle_readyz::<B>))
        .layer(middleware::from_fn(record_request))
        .with_state(state)
}

/// Instrument every request: attach/generate an `x-request-id`, log an
/// access line (HDB-124), and record latency, request count, and body bytes
/// for the known endpoints.
async fn record_request(req: Request, next: Next) -> Response {
    let rid = request_id::request_id(req.headers());
    let path = req.uri().path().to_string();
    let endpoint = match path.as_str() {
        "/query" => Some(Endpoint::Query),
        "/update" => Some(Endpoint::Update),
        "/metrics" => Some(Endpoint::Metrics),
        _ => None,
    };
    let method = if req.method() == axum::http::Method::GET {
        Method::Get
    } else {
        Method::Post
    };
    // The metrics label only distinguishes GET from "not GET"; the access
    // log names the real verb, which since SPEC-28 S5 can also be PUT/DELETE.
    let verb = req.method().as_str().to_owned();
    let start = Instant::now();

    // When the endpoint is known, wrap request and response bodies so bytes are
    // tallied as the handler reads the request and the client drains the response.
    let mut resp = if let Some(ep) = &endpoint {
        let req = {
            let (parts, body) = req.into_parts();
            let counted = CountingBody::new(body, ep.clone(), Direction::Request);
            axum::http::Request::from_parts(parts, axum::body::Body::new(counted))
        };
        let inner_resp = next.run(req).await;
        let (parts, body) = inner_resp.into_parts();
        let counted = CountingBody::new(body, ep.clone(), Direction::Response);
        axum::response::Response::from_parts(parts, axum::body::Body::new(counted))
    } else {
        next.run(req).await
    };

    let elapsed = start.elapsed();
    let status = resp.status().as_u16();
    // Every response — success or error — carries the request id so a slow
    // or failed request can be matched back to this access-log line.
    if let Ok(hv) = axum::http::HeaderValue::from_str(&rid) {
        resp.headers_mut().insert("x-request-id", hv);
    }
    eprintln!(
        "serve: {verb} {path} {status} {}ms request_id={rid}",
        elapsed.as_millis(),
    );

    if let Some(ep) = endpoint {
        let m = horndb_metrics::metrics();
        m.sparql
            .request_duration_seconds
            .get_or_create(&EndpointLabel {
                endpoint: ep.clone(),
            })
            .observe(start.elapsed().as_secs_f64());
        m.sparql
            .requests
            .get_or_create(&RequestLabels {
                endpoint: ep,
                method,
                status,
            })
            .inc();
    }
    if SERVE_WITH_INCONSISTENT_FLAG.load(Ordering::Relaxed) {
        resp.headers_mut().insert(
            axum::http::HeaderName::from_static(INCONSISTENT_HEADER),
            axum::http::HeaderValue::from_static("true"),
        );
    }
    resp
}
