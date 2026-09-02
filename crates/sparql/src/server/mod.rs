//! Embedded HTTP server exposing SPARQL 1.1 Protocol endpoints.
//!
//! Only the `/query` and `/update` endpoints. The Graph Store
//! Protocol is explicitly out of Stage 1 scope (see SPEC-07 Future
//! Work).

mod counting_body;
pub mod metrics_route;
pub mod query;
mod stream_body;
pub mod update;

use crate::exec::mem::MemStore;
use crate::exec::FullBackend;
use crate::SparqlConfig;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use counting_body::{CountingBody, Direction};
use horndb_metrics::labels::{Endpoint, EndpointLabel, Method, RequestLabels};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

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
/// `cfg` is the resolved [`SparqlConfig`] (SPEC-26 `[server.limits]`'s
/// `rdf12` and `default_graph`, PLAN-28-03 Task 2), read by both query
/// handlers.
///
/// Note: `#[derive(Clone)]` is intentionally avoided here — it would
/// wrongly require `B: Clone`. The manual impl clones only the `Arc`
/// (`cfg` is `Copy`).
pub struct AppState<B: FullBackend + Send + Sync + 'static = MemStore> {
    pub store: Arc<RwLock<B>>,
    pub cfg: SparqlConfig,
}

impl<B: FullBackend + Send + Sync + 'static> Clone for AppState<B> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            cfg: self.cfg,
        }
    }
}

/// Build the axum router. Callers attach it to a `tokio::net::TcpListener`.
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

/// Instrument every request: record latency, request count, and body bytes.
async fn record_request(req: Request, next: Next) -> Response {
    let endpoint = match req.uri().path() {
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
    let start = Instant::now();

    // When the endpoint is known, wrap request and response bodies so bytes are
    // tallied as the handler reads the request and the client drains the response.
    let resp = if let Some(ep) = &endpoint {
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
                status: resp.status().as_u16(),
            })
            .inc();
    }
    resp
}
