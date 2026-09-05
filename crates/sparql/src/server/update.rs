//! `/update` HTTP handler.

use super::AppState;
use crate::api::execute_update_with_feed;
use crate::error::SparqlError;
use crate::exec::FullBackend;
use crate::feed::FeedPosition;
use crate::SparqlConfig;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// SPEC-30 §S2: `X-HornDB-Feed-Id` / `X-HornDB-Feed-Position` headers.
/// Both present -> `Some(FeedPosition)`. Both absent -> `None` (the update
/// applies exactly as before this plan, slot untouched). Exactly one present
/// is a 400 naming both headers — the pair is all-or-nothing, so a caller
/// that forgets one gets an error rather than a silently ignored value.
fn feed_position_from_headers(headers: &HeaderMap) -> Result<Option<FeedPosition>, Box<Response>> {
    let id = headers
        .get("x-horndb-feed-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let position = headers
        .get("x-horndb-feed-position")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match (id, position) {
        (Some(id), Some(position)) => Ok(Some(FeedPosition { id, position })),
        (None, None) => Ok(None),
        _ => Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                "X-HornDB-Feed-Id and X-HornDB-Feed-Position must be given together".to_string(),
            )
                .into_response(),
        )),
    }
}

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

    let feed = match feed_position_from_headers(&headers) {
        Ok(feed) => feed,
        Err(resp) => return *resp,
    };

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
        execute_update_with_feed(
            &update,
            &mut *store,
            &SparqlConfig::default(),
            feed.as_ref(),
        )
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
        // SPEC-30 D6: a feed-id refusal is a conflict, not a bad request —
        // the request itself was well-formed, it just disagreed with the
        // slot already on record. Everything else stays a blanket 400.
        Ok(Err(e @ SparqlError::FeedIdMismatch { .. })) => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
