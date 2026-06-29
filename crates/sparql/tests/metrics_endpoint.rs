#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::server::{build_router, AppState};
use std::sync::{Arc, RwLock};
use tower::ServiceExt; // oneshot

#[tokio::test]
async fn metrics_endpoint_exposes_request_counter() {
    let state = AppState::<MemStore> {
        store: Arc::new(RwLock::new(MemStore::default())),
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
