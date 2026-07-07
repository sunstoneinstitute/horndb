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
async fn post_pattern_update_where_form() {
    // 1. Seed via INSERT DATA POSTed to /update.
    let app = router_with_data();
    let seed = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from(
            "INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> }".to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(seed).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 2. Pattern-based INSERT … WHERE: copy every <p> edge onto <q>.
    let update = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from(
            "INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }".to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(update).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 3. SELECT the freshly-inserted <s> <q> ?o triple.
    // SELECT ?o WHERE { <http://ex/s> <http://ex/q> ?o }
    let select = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Chttp%3A%2F%2Fex%2Fs%3E%20%3Chttp%3A%2F%2Fex%2Fq%3E%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(select).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"][0]["o"]["value"], "http://ex/o");
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

#[tokio::test]
async fn post_clear_default_empties_store() {
    // `CLEAR DEFAULT` over /update removes the seeded triple; a follow-up
    // SELECT returns no bindings (graph-management increment #52).
    let app = router_with_data();
    let clear = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from("CLEAR DEFAULT".to_string()))
        .unwrap();
    let resp = app.clone().oneshot(clear).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let select = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(select).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn post_load_file_inserts_triples() {
    // LOAD a file: source over /update, then SELECT it back.
    let mut path = std::env::temp_dir();
    path.push(format!("horndb_server_load_{}.nt", std::process::id()));
    std::fs::write(&path, "<http://ex/loaded> <http://ex/p> <http://ex/v> .\n").unwrap();

    let app = router_with_data();
    let load = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from(format!("LOAD <file://{}>", path.display())))
        .unwrap();
    let resp = app.clone().oneshot(load).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let select = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Chttp%3A%2F%2Fex%2Floaded%3E%20%3Chttp%3A%2F%2Fex%2Fp%3E%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(select).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"][0]["o"]["value"], "http://ex/v");
    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn explain_pragma_returns_text_plan() {
    let app = router_with_data();
    // EXPLAIN SELECT ?o WHERE { ?s ?p ?o }
    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/sparql-query")
        .body(Body::from(
            "EXPLAIN SELECT ?o WHERE { ?s ?p ?o }".to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(ctype.starts_with("text/plain"), "content-type: {ctype}");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("mode: materialized"), "{text}");
    assert!(text.contains("BgpScan"), "{text}");
}

/// Parse a prometheus-client text-format counter value for a metric line that
/// starts with `metric_name` and contains `label_substr`.
fn parse_counter(output: &str, metric_name: &str, label_substr: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        if line.starts_with(metric_name) && line.contains(label_substr) && !line.starts_with('#') {
            line.split_whitespace().last()?.parse::<u64>().ok()
        } else {
            None
        }
    })
}

#[tokio::test]
async fn byte_counters_are_incremented() {
    // POST a SELECT query to /query so the response has an actual JSON body
    // (unlike /update which returns 204 No Content with no body).
    let app = router_with_data();
    let body_str = "SELECT ?o WHERE { ?s ?p ?o }";
    let body_len = body_str.len() as u64;

    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/sparql-query")
        .header("accept", "application/sparql-results+json")
        .body(Body::from(body_str))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Drain the response body so the response CountingBody fires its observation.
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(
        !body.is_empty(),
        "response body must be non-empty for this test"
    );

    let metrics = horndb_metrics::encode_metrics();
    let req_bytes = parse_counter(
        &metrics,
        "horndb_sparql_request_bytes_total",
        r#"endpoint="query""#,
    )
    .unwrap_or(0);
    let resp_bytes = parse_counter(
        &metrics,
        "horndb_sparql_response_bytes_total",
        r#"endpoint="query""#,
    )
    .unwrap_or(0);

    assert!(
        req_bytes >= body_len,
        "expected request_bytes >= {body_len}, got {req_bytes}\nmetrics:\n{metrics}"
    );
    assert!(
        resp_bytes >= 1,
        "expected response_bytes >= 1, got {resp_bytes}\nmetrics:\n{metrics}"
    );
}

#[tokio::test]
async fn explain_json_pragma_returns_json_plan() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/sparql-query")
        .body(Body::from(
            "EXPLAIN JSON SELECT ?o WHERE { ?s ?p ?o }".to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        ctype.starts_with("application/json"),
        "content-type: {ctype}"
    );
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["mode"], "materialized");
    assert!(v["plan"]["op"].is_string());
}

/// 5000 rows is above the fixed release batch_rows() of 4096, so a streamed
/// body must arrive in >= 2 data frames. One frame == the old materialized
/// path (this is the memory-win mechanism proof: multiple frames means the
/// full serialized document never existed in one buffer).
#[tokio::test]
async fn large_select_streams_in_multiple_chunks() {
    use http_body::Body as _;

    let mut s = MemStore::default();
    for i in 0..5000 {
        s.insert_triple(
            iri(&format!("http://ex/s{i}")),
            iri("http://ex/p"),
            iri(&format!("http://ex/o{i}")),
        );
    }
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
    };
    let app = build_router(state);

    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fs%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["content-type"],
        "application/sparql-results+json"
    );

    let mut body = resp.into_body();
    let mut frames = 0usize;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.expect("clean stream");
        if let Ok(data) = frame.into_data() {
            frames += 1;
            buf.extend_from_slice(&data);
        }
    }
    assert!(
        frames >= 2,
        "expected a chunked body, got {frames} frame(s)"
    );
    let v: serde_json::Value =
        serde_json::from_slice(&buf).expect("frames concatenate to valid JSON");
    assert_eq!(v["results"]["bindings"].as_array().unwrap().len(), 5000);
}

/// A SELECT whose whole result fits in the first operator chunk must come
/// back as a plain sized body (Content-Length, one data frame) — the
/// streaming machinery (chunked body, channel) is skipped so small results
/// pay no per-query overhead vs the materialized path.
#[tokio::test]
async fn small_select_replies_with_sized_single_frame_body() {
    use http_body::Body as _;

    let mut s = MemStore::default();
    for i in 0..3 {
        s.insert_triple(
            iri(&format!("http://ex/s{i}")),
            iri("http://ex/p"),
            iri(&format!("http://ex/o{i}")),
        );
    }
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
    };
    let app = build_router(state);

    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fs%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let content_length: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    let mut body = resp.into_body();
    let mut frames = 0usize;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.expect("clean body");
        if let Ok(data) = frame.into_data() {
            frames += 1;
            buf.extend_from_slice(&data);
        }
    }
    assert_eq!(frames, 1, "single-chunk result must be one sized frame");
    assert_eq!(
        content_length,
        Some(buf.len() as u64),
        "single-chunk result must carry Content-Length"
    );
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(v["results"]["bindings"].as_array().unwrap().len(), 3);
}

mod streaming_error_semantics {
    use super::*;
    use horndb_sparql::algebra::{TriplePattern, Var};
    use horndb_sparql::exec::{Batch, Bindings, Executor, Row, Slot};
    use horndb_sparql::SparqlError;
    use horndb_storage::TermId;

    /// Backend whose scan fails immediately: the error lands before the
    /// first chunk, so the response must be a clean 400.
    struct FailingScan;

    impl Executor for FailingScan {
        fn scan_bgp(
            &self,
            _patterns: &[TriplePattern],
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            Err(SparqlError::Executor("scan exploded".into()))
        }
    }
    impl horndb_sparql::exec::Store for FailingScan {
        fn insert_triple(&mut self, _s: Term, _p: Term, _o: Term) {}
        fn delete_triple(&mut self, _s: &Term, _p: &Term, _o: &Term) {}
        fn clear_all(&mut self) {}
    }

    #[tokio::test]
    async fn exec_error_before_first_chunk_returns_400() {
        let state = AppState {
            store: Arc::new(RwLock::new(FailingScan)),
        };
        let app = build_router(state);
        let req = Request::builder()
            .uri("/query?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 5000 id-rows; decoding any id >= 4096 fails. Chunk 1 (4096 rows)
    /// serializes and commits the 200; the failure lands in chunk 2, so the
    /// body must abort mid-stream (protocol-level truncation), NOT morph
    /// into an error status.
    struct DecodeFailsLate;

    impl Executor for DecodeFailsLate {
        fn scan_bgp(
            &self,
            _patterns: &[TriplePattern],
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            unreachable!("scan_bgp_ids is overridden")
        }
        fn scan_bgp_ids(&self, _patterns: &[TriplePattern]) -> horndb_sparql::Result<Batch> {
            Ok(Batch {
                schema: vec![Var::new("s"), Var::new("p"), Var::new("o")],
                rows: (0u64..5000)
                    .map(|i| {
                        Row(vec![
                            Slot::Id(TermId(i)),
                            Slot::Id(TermId(i)),
                            Slot::Id(TermId(i)),
                        ])
                    })
                    .collect(),
            })
        }
        fn decode_term(&self, id: TermId) -> horndb_sparql::Result<Term> {
            if id.0 < 4096 {
                Ok(Term::Iri(format!("http://ex/t{}", id.0)))
            } else {
                Err(SparqlError::Executor("decode failed mid-stream".into()))
            }
        }
    }
    impl horndb_sparql::exec::Store for DecodeFailsLate {
        fn insert_triple(&mut self, _s: Term, _p: Term, _o: Term) {}
        fn delete_triple(&mut self, _s: &Term, _p: &Term, _o: &Term) {}
        fn clear_all(&mut self) {}
    }

    #[tokio::test]
    async fn exec_error_mid_stream_aborts_body_after_200() {
        use http_body::Body as _;

        let state = AppState {
            store: Arc::new(RwLock::new(DecodeFailsLate)),
        };
        let app = build_router(state);
        // SELECT all three vars so column pruning keeps every column.
        let req = Request::builder()
            .uri(
                "/query?query=SELECT%20%3Fs%20%3Fp%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D",
            )
            .header("accept", "text/csv")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "headers are already committed when the error hits"
        );

        let mut body = resp.into_body();
        let mut data_frames = 0usize;
        let mut saw_error = false;
        while let Some(frame) =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            match frame {
                Ok(f) => {
                    if f.into_data().is_ok() {
                        data_frames += 1;
                    }
                }
                Err(_) => {
                    saw_error = true;
                    break;
                }
            }
        }
        assert!(data_frames >= 1, "chunk 1 was delivered before the error");
        assert!(saw_error, "the body must surface the mid-stream error");
    }
}
