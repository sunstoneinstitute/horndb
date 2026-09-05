//! `GET /healthz` and `GET /readyz` — Kubernetes liveness/readiness probes
//! (HDB-124).
//!
//! `/healthz` is unconditional 200: it only proves the process is up and the
//! axum event loop is answering requests, so a liveness probe won't kill a
//! server that is still (correctly) loading data. `/readyz` reflects
//! `AppState.ready`, flipped once the `serve` binary's startup load (and any
//! `--materialize` pass) finishes — 503 before that, so a Kubernetes
//! readiness probe keeps the pod out of the load-balancer pool during a
//! multi-minute cold load with no persistence yet.

use super::AppState;
use crate::exec::FullBackend;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::sync::atomic::Ordering;

pub async fn handle_healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

pub async fn handle_readyz<B: FullBackend + Send + Sync + 'static>(
    State(state): State<AppState<B>>,
) -> impl IntoResponse {
    if state.ready.load(Ordering::Acquire) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "loading")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::mem::MemStore;
    use axum::body::Body;
    use axum::http::Request;
    use parking_lot::RwLock;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn state(ready: bool) -> AppState<MemStore> {
        AppState {
            store: Arc::new(RwLock::new(MemStore::default())),
            config: Default::default(),
            ready: Arc::new(AtomicBool::new(ready)),
            admission: Default::default(),
        }
    }

    #[tokio::test]
    async fn healthz_is_always_ok() {
        let app = super::super::build_router(state(false));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_reflects_the_ready_flag() {
        let not_ready = super::super::build_router(state(false));
        let resp = not_ready
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let ready = super::super::build_router(state(true));
        let resp = ready
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// HDB-144: the data endpoints must shed while the store is still
    /// loading, not answer from a partly-loaded corpus. Without this, a
    /// benchmark driver that waits on `/query` measures a fraction of the
    /// data and reports it as a result.
    #[tokio::test]
    async fn query_and_update_shed_until_ready() {
        for (uri, body) in [("/query?query=ASK%7B%7D", ""), ("/update", "")] {
            let app = super::super::build_router(state(false));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method(if uri == "/update" { "POST" } else { "GET" })
                        .uri(uri)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{uri} should shed while loading"
            );
            assert_eq!(
                resp.headers()
                    .get("retry-after")
                    .map(|v| v.to_str().unwrap()),
                Some("1"),
                "{uri} should tell the caller when to retry"
            );
        }

        // Same query once the load has finished.
        let app = super::super::build_router(state(true));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/query?query=ASK%7B%7D")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
