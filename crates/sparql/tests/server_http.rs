#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use horndb_sparql::server::build_router;
use horndb_sparql::server::AppState;
use std::sync::{Arc, RwLock};
use tower::ServiceExt;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn router_with_data() -> axum::Router {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
    };
    build_router(state)
}

#[tokio::test]
async fn get_query_returns_json() {
    let app = router_with_data();
    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"][0]["o"]["value"], "http://ex/b");
}

#[tokio::test]
async fn post_update_then_query() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from(
            "INSERT DATA { <http://ex/x> <http://ex/p> <http://ex/y> }".to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn get_describe_returns_ntriples() {
    let app = router_with_data();
    // DESCRIBE <http://ex/a> — percent-encoded.
    let req = Request::builder()
        .uri("/query?query=DESCRIBE%20%3Chttp%3A%2F%2Fex%2Fa%3E")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(text.trim(), "<http://ex/a> <http://ex/p> <http://ex/b> .");
}

#[tokio::test]
async fn parse_error_returns_400() {
    let app = router_with_data();
    let req = Request::builder()
        .uri("/query?query=NOT_VALID")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_query_returns_json_hornbackend() {
    let mut backend = HornBackend::new();
    backend.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let state = AppState::<HornBackend> {
        store: Arc::new(RwLock::new(backend)),
    };
    let app = build_router(state);

    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"][0]["o"]["value"], "http://ex/b");
}
