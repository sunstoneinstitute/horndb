#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::server::{build_router, AppState};
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tower::ServiceExt; // oneshot

#[tokio::test]
async fn metrics_endpoint_exposes_request_counter() {
    let state = AppState::<MemStore> {
        store: Arc::new(RwLock::new(MemStore::default())),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    };
    let app = build_router(state);

    // SELECT ?o WHERE { ?s ?p ?o } — percent-encoded, matching the
    // existing server tests' approach (no `urlencoding` dev-dep).
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("horndb_sparql_requests_total"),
        "got:\n{text}"
    );
    assert!(text.contains("endpoint=\"query\""), "got:\n{text}");
}

/// HDB-166: `/graphs` (the Graph Store Protocol) was not in the `Endpoint`
/// label enum, so it produced none of `sparql_requests`,
/// `request_duration_seconds` or the body-byte counters — only the access
/// log. Same shape as the `/query` case above, on a route with no graph and
/// no body, so the response is a plain 404.
#[tokio::test]
async fn graphs_endpoint_emits_request_and_duration_metrics() {
    let state = AppState::<MemStore> {
        store: Arc::new(RwLock::new(MemStore::default())),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    };
    let app = build_router(state);

    // The response body must be fully drained for `CountingBody` to observe
    // it — it tallies bytes on end-of-stream, not on the handler returning.
    let graphs_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/graphs?graph=http%3A%2F%2Fex%2Fg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = axum::body::to_bytes(graphs_resp.into_body(), usize::MAX)
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("horndb_sparql_requests_total") && text.contains("endpoint=\"graphs\""),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_sparql_request_duration_seconds")
            && text.contains("endpoint=\"graphs\""),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_sparql_response_bytes_total") && text.contains("endpoint=\"graphs\""),
        "got:\n{text}"
    );
}
