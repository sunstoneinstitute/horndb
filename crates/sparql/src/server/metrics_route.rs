//! `GET /metrics` — OpenMetrics text exposition for Prometheus scrape.

use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;

/// `GET /metrics` — OpenMetrics text for Prometheus scrape.
pub async fn handle_metrics() -> impl IntoResponse {
    let body = horndb_metrics::encode_metrics();
    (
        [(
            CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        body,
    )
}
