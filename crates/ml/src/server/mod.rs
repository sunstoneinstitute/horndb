//! HTTP boundary for the ML integration surface (SPEC-08 F3 + F6).
//!
//! Two endpoints, both opt-in and both fail-closed when ML is disabled:
//!
//! * `POST /nl-query` (F3) — translate a natural-language question to
//!   SPARQL via the registered [`Translator`](crate::nlquery::Translator),
//!   execute it via the injected [`SparqlExecutor`](crate::nlquery::SparqlExecutor),
//!   and return the generated SPARQL alongside the results. The SPARQL is
//!   **always** returned so a human can audit/correct it.
//! * `GET /ml-audit` (F6) — paginated list of ML-derived facts admitted in
//!   a time window, with model id + confidence.
//!
//! The router is intentionally self-contained in `horndb-ml`: it does not
//! depend on the full SPARQL/storage stack. The real engine is plugged in
//! at the call site by supplying a `SparqlExecutor` that forwards to
//! SPEC-07; tests supply a fake. That keeps `cargo test` hermetic.

mod audit;
mod nlquery;

use crate::nlquery::SparqlExecutor;
use crate::registry::MlRegistry;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

/// Shared state for the ML HTTP endpoints.
///
/// Cheap to clone (two `Arc`s). The `registry` owns the translator,
/// audit log, and privacy policy; the `executor` runs generated SPARQL.
#[derive(Clone)]
pub struct MlAppState {
    pub registry: Arc<MlRegistry>,
    pub executor: Arc<dyn SparqlExecutor>,
}

impl MlAppState {
    pub fn new(registry: Arc<MlRegistry>, executor: Arc<dyn SparqlExecutor>) -> Self {
        Self { registry, executor }
    }
}

/// Build the axum router for the ML endpoints. Callers mount it on their
/// own listener (or merge it into a larger router with `Router::merge`).
pub fn build_router(state: MlAppState) -> Router {
    Router::new()
        .route("/nl-query", post(nlquery::handle_nl_query))
        .route("/ml-audit", get(audit::handle_ml_audit))
        .with_state(state)
}
