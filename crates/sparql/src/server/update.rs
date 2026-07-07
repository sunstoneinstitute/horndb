//! `/update` HTTP handler.

use super::AppState;
use crate::api::execute_update;
use crate::exec::FullBackend;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use std::sync::Arc;

pub async fn handle_update<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let update = if ctype.contains("application/x-www-form-urlencoded") {
        match super::query::url_form_field(&body, "update") {
            Some(u) => u,
            None => {
                return (StatusCode::BAD_REQUEST, "form missing `update`".to_string())
                    .into_response()
            }
        }
    } else {
        body
    };

    // Streamed SELECTs (`/query`) hold the store read lock for
    // client-controlled durations (until the client drains the body), so
    // `write()` can block for a long time. Never block a tokio runtime
    // worker on it: park the lock wait AND the update execution on the
    // blocking pool. Otherwise one stalled streaming client plus N
    // concurrent updates (N = worker threads) wedges every runtime worker
    // in `write()`, nothing polls connections, the streamed body never
    // drains, and the read lock never releases — deadlock.
    let store = Arc::clone(&state.store);
    let result = tokio::task::spawn_blocking(move || {
        let mut store = store.write().unwrap();
        execute_update(&update, &mut *store)
    })
    .await
    .expect("update task panicked");

    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
