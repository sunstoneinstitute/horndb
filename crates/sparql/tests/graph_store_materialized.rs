//! SPEC-28 S5 — `?default` is refused for GSP writes on a `--materialize`
//! store, because `load_with_reasoning` puts asserted and inferred triples
//! into the default graph indistinguishably.
//!
//! Its own test binary: `flag_materialized()` is a one-way process-global,
//! so it would leak into every other test sharing the process.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use horndb_sparql::server::{build_router, flag_materialized, AppState};
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tower::ServiceExt;

async fn send(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "text/turtle")
                .body(Body::from("<http://ex/a> <http://ex/p> <http://ex/o> ."))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn default_graph_writes_are_refused_on_a_materialized_store() {
    flag_materialized();

    let mut store = MemStore::default();
    store.insert_triple(
        horndb_sparql::algebra::Term::Iri("http://ex/s".into()),
        horndb_sparql::algebra::Term::Iri("http://ex/p".into()),
        horndb_sparql::algebra::Term::Iri("http://ex/o".into()),
    );
    let app = build_router(AppState {
        store: Arc::new(RwLock::new(store)),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    });

    for method in ["PUT", "POST", "DELETE"] {
        let (status, body) = send(&app, method, "/graphs?default").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method}");
        assert!(
            body.contains("--materialize"),
            "{method} must name the reason: {body}"
        );
    }

    // Reads of the default graph stay allowed, and a named graph is still
    // writable — the restriction is scoped to `?default`.
    assert_eq!(send(&app, "GET", "/graphs?default").await.0, StatusCode::OK);
    assert_eq!(
        send(&app, "PUT", "/graphs?graph=http%3A%2F%2Fex%2Fg")
            .await
            .0,
        StatusCode::CREATED
    );
}
