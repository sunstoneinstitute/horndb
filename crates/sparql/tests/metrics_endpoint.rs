#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
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

/// SPEC-25 S5 / HDB-178: `storage_tier_bytes_estimated` must report real
/// per-tier byte counts once a store has cold, memory-mapped partitions —
/// not the placeholder `tier="unknown"` series it used to.
///
/// The storage collector is only wired up in `bin/serve.rs::run`, so this
/// test registers its own, exactly as `run` does, over a `HornBackend`-backed
/// store it demotes before scraping.
#[tokio::test]
async fn metrics_endpoint_reports_dram_and_cold_tier_bytes() {
    let mut backend = HornBackend::new();
    backend.insert_triple(
        Term::Iri("http://ex/a".into()),
        Term::Iri("http://ex/p".into()),
        Term::Iri("http://ex/b".into()),
    );
    backend.demote_all().expect("demote_all");
    let store = Arc::new(RwLock::new(backend));

    let store_weak = Arc::downgrade(&store);
    horndb_metrics::register_collector(Box::new(horndb_metrics::storage::StorageCollector::new(
        move || {
            let arc = store_weak.upgrade()?;
            let guard = arc.read();
            let s = guard.storage_stats();
            Some(horndb_metrics::storage::StorageSnapshot {
                triples: s.triples as i64,
                graphs: s.graphs as i64,
                predicates: s.predicates as i64,
                dictionary_terms: s.dictionary_terms as i64,
                dictionary_terms_live: s.dictionary_terms_live as i64,
                dictionary_bytes: s.dictionary_bytes as i64,
                tier_bytes_warm: (s.bytes_estimated - s.bytes_cold) as i64,
                tier_bytes_cold: s.bytes_cold as i64,
            })
        },
    )));

    let state = AppState::<HornBackend> {
        store,
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    };
    let app = build_router(state);

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
        text.contains("horndb_storage_tier_bytes_estimated{tier=\"dram\"}"),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_storage_tier_bytes_estimated{tier=\"cold\"}"),
        "got:\n{text}"
    );
    // Not just the series name: the backend was demoted above, so the cold
    // value has to be real. A `bytes_cold` stuck at 0 still emits the series.
    let cold: u64 = text
        .lines()
        .find_map(|l| {
            l.strip_prefix("horndb_storage_tier_bytes_estimated{tier=\"cold\"} ")?
                .trim()
                .parse()
                .ok()
        })
        .expect("cold series carries a numeric value");
    assert!(cold > 0, "demoted store must report non-zero cold bytes");
}
