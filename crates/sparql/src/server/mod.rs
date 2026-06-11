//! Embedded HTTP server exposing SPARQL 1.1 Protocol endpoints.
//!
//! Only the `/query` and `/update` endpoints. The Graph Store
//! Protocol is explicitly out of Stage 1 scope (see SPEC-07 Future
//! Work).

pub mod query;
pub mod update;

use crate::exec::mem::MemStore;
use crate::exec::FullBackend;
use axum::routing::{get, post};
use axum::Router;
use std::sync::{Arc, RwLock};

/// Shared state, generic over the storage backend. Defaults to the
/// Stage-1 `MemStore` so existing constructors keep compiling; the
/// `serve` binary instantiates `AppState<HornBackend>`.
///
/// The store is wrapped in an `RwLock` so concurrent SPARQL queries
/// take the read lock and run in parallel, while SPARQL Update takes
/// the write lock. SPEC-02 will replace this with MVCC.
///
/// Note: `#[derive(Clone)]` is intentionally avoided here — it would
/// wrongly require `B: Clone`. The manual impl clones only the `Arc`.
pub struct AppState<B: FullBackend + Send + Sync + 'static = MemStore> {
    pub store: Arc<RwLock<B>>,
}

impl<B: FullBackend + Send + Sync + 'static> Clone for AppState<B> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
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
        .with_state(state)
}
