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
    // Refuse to mutate a partly-loaded store (HDB-144); see
    // `AppState::shed_while_loading`.
    if let Some(resp) = state.shed_while_loading() {
        return resp;
    }

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

    // Since HDB-119 a streamed SELECT holds no lock while it streams, so
    // `write()` no longer waits on a slow client. It can still wait on
    // another update, or on a materialized query's execution, so keep both
    // the lock wait and the update itself off the runtime workers: a tokio
    // worker blocked in `write()` polls no connections.
    let store = Arc::clone(&state.store);
    let result = tokio::task::spawn_blocking(move || {
        let mut store = store.write();
        execute_update(&update, &mut *store)
    })
    .await;

    match result {
        // `JoinError` here means the blocking task panicked (or was
        // cancelled). The store lock never poisons (parking_lot, HDB-114),
        // so this request fails cleanly and later requests are unaffected.
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "update task panicked".to_string(),
        )
            .into_response(),
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
