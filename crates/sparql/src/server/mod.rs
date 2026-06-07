//! Embedded HTTP server exposing SPARQL 1.1 Protocol endpoints.
//!
//! Only the `/query` and `/update` endpoints. The Graph Store
//! Protocol is explicitly out of Stage 1 scope (see SPEC-07 Future
//! Work).

pub mod query;
pub mod update;

use crate::exec::mem::MemStore;
use axum::routing::{get, post};
use axum::Router;
use std::sync::{Arc, RwLock};

/// Shared state: the store is wrapped in an `RwLock` so concurrent
/// SPARQL queries take the read lock and run in parallel, while SPARQL
/// Update takes the write lock. `MemStore` is not internally
/// synchronised. SPEC-02 will replace this with MVCC.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<RwLock<MemStore>>,
}

/// Build the axum router. Callers attach it to a `tokio::net::TcpListener`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/query",
            get(query::handle_query_get).post(query::handle_query_post),
        )
        .route("/update", post(update::handle_update))
        .with_state(state)
}
