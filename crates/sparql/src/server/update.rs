//! `/update` HTTP handler.

use super::AppState;
use crate::api::execute_update;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

pub async fn handle_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let update = if ctype.contains("application/x-www-form-urlencoded") {
        match super::query::url_form_update(&body) {
            Some(u) => u,
            None => {
                return (StatusCode::BAD_REQUEST, "form missing `update`".to_string())
                    .into_response()
            }
        }
    } else {
        body
    };

    let mut store = state.store.write().unwrap();
    match execute_update(&update, &mut *store) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
