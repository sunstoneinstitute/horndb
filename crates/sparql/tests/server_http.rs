#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::ScanScope;
use horndb_sparql::exec::Store;
use horndb_sparql::server::build_router;
use horndb_sparql::server::AppState;
use horndb_sparql::SparqlConfig;
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tower::ServiceExt;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn router_with_data() -> axum::Router {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
        cfg: SparqlConfig::default(),
        ready: Arc::new(AtomicBool::new(true)),
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

/// A router whose store holds `<http://ex/a> <p> <b>` in the default graph
/// and `<http://ex/g_only> <p> <b>` in the named graph `<http://ex/g>` —
/// enough to tell scoped answers apart from whole-store ones (SPEC-28 S3).
fn router_with_named_graph() -> axum::Router {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_quad(
        Some("http://ex/g"),
        (
            "http://ex/g_only".into(),
            "http://ex/p".into(),
            "http://ex/b".into(),
        ),
    );
    let state = AppState {
        store: Arc::new(RwLock::new(s)),
        cfg: SparqlConfig::default(),
        ready: Arc::new(AtomicBool::new(true)),
    };
    build_router(state)
}

/// POST `q` and return the parsed JSON body, asserting a 200.
async fn post_query_json(app: axum::Router, q: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/sparql-query")
        .header("accept", "application/sparql-results+json")
        .body(Body::from(q.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "query should answer: {q}");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// `GRAPH <g>` is evaluated, not refused (it 400'd through SPEC-28 phase 1).
/// Exercises the streaming-SELECT path (`plan_select`, `query.rs`).
#[tokio::test]
async fn graph_query_scopes_to_the_named_graph() {
    let v = post_query_json(
        router_with_named_graph(),
        "SELECT ?s WHERE { GRAPH <http://ex/g> { ?s ?p ?o } }",
    )
    .await;
    let b = &v["results"]["bindings"];
    assert_eq!(b.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(b[0]["s"]["value"], "http://ex/g_only");
}

/// A `FROM` clause builds the default graph out of exactly the named
/// graphs, so the default-graph row is *not* in the answer.
#[tokio::test]
async fn from_query_builds_the_default_graph_from_the_named_graphs() {
    let v = post_query_json(
        router_with_named_graph(),
        "SELECT ?s FROM <http://ex/g> WHERE { ?s ?p ?o }",
    )
    .await;
    let b = &v["results"]["bindings"];
    assert_eq!(b.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(b[0]["s"]["value"], "http://ex/g_only");
}

/// Same invariant for ASK: `plan_select` only recognizes `SELECT`, so ASK
/// falls through to `run_materialized` — the other server-side path.
#[tokio::test]
async fn ask_graph_query_answers_over_the_named_graph() {
    let v = post_query_json(
        router_with_named_graph(),
        "ASK { GRAPH <http://ex/g> { ?s ?p ?o } }",
    )
    .await;
    assert_eq!(v["boolean"], true, "{v}");
    let v = post_query_json(
        router_with_named_graph(),
        "ASK { GRAPH <http://ex/absent> { ?s ?p ?o } }",
    )
    .await;
    assert_eq!(
        v["boolean"], false,
        "an unknown graph is false, not an error"
    );
}

/// SPEC-26 S4 / SPEC-28 S3/D2: the `default_graph` per-query override
/// (`union`/`strict`) is parsed next to `query` in the POST-form channel. An
/// unrecognized value is a 400 naming the offending key. This pins the
/// parse/validate contract only — PLAN-28-03 Task 2's scope. The full
/// behavioural assertion (that `default_graph=strict` actually changes
/// which rows come back) lands in Task 3, once the executor consumes
/// `SparqlConfig::default_graph` (currently threaded but not yet read).
#[tokio::test]
async fn default_graph_url_param_form_bad_value_returns_400_naming_key() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D&default_graph=bogus"
                .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("default_graph"),
        "body should name the offending key: {text}"
    );
}

/// A valid `default_graph` value changes which rows come back: over a store
/// with one default-graph and one named-graph triple, `union` sees both and
/// `strict` sees only the default graph (SPEC-28 D2).
#[tokio::test]
async fn default_graph_url_param_form_valid_value_changes_the_result_set() {
    let subjects = |mode: &str| {
        let body = format!(
            "query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D&default_graph={mode}"
        );
        let app = router_with_named_graph();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri("/query")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/sparql-results+json")
                .body(Body::from(body))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let mut out: Vec<String> = v["results"]["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["s"]["value"].as_str().unwrap().to_owned())
                .collect();
            out.sort();
            out
        }
    };
    assert_eq!(
        subjects("union").await,
        vec!["http://ex/a", "http://ex/g_only"],
        "union sees every non-reserved graph"
    );
    assert_eq!(
        subjects("strict").await,
        vec!["http://ex/a"],
        "strict sees only the default-graph sentinel"
    );
}

/// The form-encoded POST-body field wins over a `default_graph` also present
/// on the URL query string: the body carries a *valid* value (`union`) while
/// the URL carries an *invalid* one (`bogus`) — a 200 here proves the body's
/// value was applied and the URL's was never even parsed.
#[tokio::test]
async fn default_graph_url_param_form_body_field_wins_over_url() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query?default_graph=bogus")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/sparql-results+json")
        .body(Body::from(
            "query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D&default_graph=union"
                .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// When the form body carries no `default_graph` field at all, the handler
/// falls back to the URL query string — same channel GET and direct-POST
/// use. Before this fix the fallback did not exist: a form-encoded POST with
/// `default_graph` *only* on the URL silently got `union` semantics (200,
/// no error) instead of this 400.
#[tokio::test]
async fn default_graph_url_param_form_falls_back_to_url_when_absent_from_body() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query?default_graph=bogus")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D".to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("default_graph"),
        "body should name the offending key: {text}"
    );
}

/// The GET-channel counterpart: same invalid-value contract (400 naming the
/// key), proven on the channel that reads `default_graph` next to `query`
/// (`QueryParams`, GET's `Query` extractor).
#[tokio::test]
async fn default_graph_url_param_get_bad_value_returns_400_naming_key() {
    let app = router_with_data();
    let req = Request::builder()
        .uri(
            "/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D&default_graph=bogus",
        )
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("default_graph"),
        "body should name the offending key: {text}"
    );
}

/// The direct-POST channel (`application/sparql-query`, raw body = the query
/// text): per SPARQL 1.1 Protocol §2.1.2 this client puts `default_graph` on
/// the URL query string, not in the body — same as GET.
#[tokio::test]
async fn default_graph_url_param_direct_post_bad_value_returns_400_naming_key() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query?default_graph=bogus")
        .header("content-type", "application/sparql-query")
        .body(Body::from("SELECT ?o WHERE { ?s ?p ?o }".to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("default_graph"),
        "body should name the offending key: {text}"
    );
}

/// The direct-POST channel's positive case: a valid `?default_graph=` value
/// must run the query (not 400), proving the override is actually read on
/// this branch now, not merely rejected when malformed.
#[tokio::test]
async fn default_graph_url_param_direct_post_valid_value_runs_query() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/query?default_graph=strict")
        .header("content-type", "application/sparql-query")
        .header("accept", "application/sparql-results+json")
        .body(Body::from("SELECT ?o WHERE { ?s ?p ?o }".to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_query_returns_json_hornbackend() {
    let mut backend = HornBackend::new();
    backend.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let state = AppState::<HornBackend> {
        store: Arc::new(RwLock::new(backend)),
        cfg: SparqlConfig::default(),
        ready: Arc::new(AtomicBool::new(true)),
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
        cfg: SparqlConfig::default(),
        ready: Arc::new(AtomicBool::new(true)),
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
        cfg: SparqlConfig::default(),
        ready: Arc::new(AtomicBool::new(true)),
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
            _scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            Err(SparqlError::Executor("scan exploded".into()))
        }
    }
    impl horndb_sparql::exec::Store for FailingScan {
        fn apply_quads(
            &mut self,
            _dels: Vec<horndb_sparql::exec::AlgebraQuad>,
            _adds: Vec<horndb_sparql::exec::AlgebraQuad>,
        ) -> horndb_sparql::Result<horndb_sparql::exec::ApplyCounts> {
            Ok(horndb_sparql::exec::ApplyCounts::default())
        }
        fn clear_graph(
            &mut self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<usize> {
            Ok(0)
        }
        fn graph_exists(&self, _graph: &str) -> bool {
            false
        }
        fn graphs(&self) -> Vec<String> {
            Vec::new()
        }
        fn scan_graph_quads(
            &self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<Vec<horndb_sparql::exec::AlgebraTriple>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn exec_error_before_first_chunk_returns_400() {
        let state = AppState {
            store: Arc::new(RwLock::new(FailingScan)),
            cfg: SparqlConfig::default(),
            ready: Arc::new(AtomicBool::new(true)),
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
            _scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            unreachable!("scan_bgp_ids is overridden")
        }
        fn scan_bgp_ids(
            &self,
            _patterns: &[TriplePattern],
            _scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Batch> {
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
        fn apply_quads(
            &mut self,
            _dels: Vec<horndb_sparql::exec::AlgebraQuad>,
            _adds: Vec<horndb_sparql::exec::AlgebraQuad>,
        ) -> horndb_sparql::Result<horndb_sparql::exec::ApplyCounts> {
            Ok(horndb_sparql::exec::ApplyCounts::default())
        }
        fn clear_graph(
            &mut self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<usize> {
            Ok(0)
        }
        fn graph_exists(&self, _graph: &str) -> bool {
            false
        }
        fn graphs(&self) -> Vec<String> {
            Vec::new()
        }
        fn scan_graph_quads(
            &self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<Vec<horndb_sparql::exec::AlgebraTriple>> {
            Ok(Vec::new())
        }
    }

    /// Same shape as `DecodeFailsLate`, but the failure is a *panic*, not a
    /// `SparqlError`, and it lands in chunk 3 — past the chunk-2 peek, so
    /// the 200 is already committed and the panic unwinds the blocking
    /// serializer with the body live. Without the abort guard the sender
    /// would just be dropped, cleanly terminating a truncated document.
    /// (A panic in chunk 1 or in the chunk-2 peek is still a clean 500:
    /// no bytes have been emitted yet.)
    struct PanicsLate;

    impl Executor for PanicsLate {
        fn scan_bgp(
            &self,
            _patterns: &[TriplePattern],
            _scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            unreachable!("scan_bgp_ids is overridden")
        }
        fn scan_bgp_ids(
            &self,
            _patterns: &[TriplePattern],
            _scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Batch> {
            Ok(Batch {
                schema: vec![Var::new("s"), Var::new("p"), Var::new("o")],
                // 9000 rows over a 4096-row batch: chunks 1 and 2 are clean,
                // chunk 3 panics.
                rows: (0u64..9000)
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
            assert!(id.0 < 8192, "injected serializer panic mid-stream");
            Ok(Term::Iri(format!("http://ex/t{}", id.0)))
        }
    }
    impl horndb_sparql::exec::Store for PanicsLate {
        fn apply_quads(
            &mut self,
            _dels: Vec<horndb_sparql::exec::AlgebraQuad>,
            _adds: Vec<horndb_sparql::exec::AlgebraQuad>,
        ) -> horndb_sparql::Result<horndb_sparql::exec::ApplyCounts> {
            Ok(horndb_sparql::exec::ApplyCounts::default())
        }
        fn clear_graph(
            &mut self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<usize> {
            Ok(0)
        }
        fn graph_exists(&self, _graph: &str) -> bool {
            false
        }
        fn graphs(&self) -> Vec<String> {
            Vec::new()
        }
        fn scan_graph_quads(
            &self,
            _graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<Vec<horndb_sparql::exec::AlgebraTriple>> {
            Ok(Vec::new())
        }
    }

    /// HDB-115: a panic in the blocking serializer must abort the body, not
    /// hand the client a well-formed short CSV under a 200.
    #[tokio::test]
    async fn serializer_panic_mid_stream_aborts_body() {
        use http_body::Body as _;

        let state = AppState {
            store: Arc::new(RwLock::new(PanicsLate)),
            cfg: SparqlConfig::default(),
            ready: Arc::new(AtomicBool::new(true)),
        };
        let app = build_router(state);
        let req = Request::builder()
            .uri(
                "/query?query=SELECT%20%3Fs%20%3Fp%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D",
            )
            .header("accept", "text/csv")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "headers already committed");

        let mut body = resp.into_body();
        let mut delivered: Vec<u8> = Vec::new();
        let mut saw_error = false;
        while let Some(frame) =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
        {
            match frame {
                Ok(f) => {
                    if let Ok(data) = f.into_data() {
                        delivered.extend_from_slice(&data);
                    }
                }
                Err(_) => {
                    saw_error = true;
                    break;
                }
            }
        }
        assert!(
            saw_error,
            "a serializer panic must abort the body; instead the client got a \
             cleanly terminated {} byte document",
            delivered.len()
        );
        assert!(
            !delivered.is_empty(),
            "chunk 1 was delivered before the panic"
        );
    }

    #[tokio::test]
    async fn exec_error_mid_stream_aborts_body_after_200() {
        use http_body::Body as _;

        let state = AppState {
            store: Arc::new(RwLock::new(DecodeFailsLate)),
            cfg: SparqlConfig::default(),
            ready: Arc::new(AtomicBool::new(true)),
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

/// HDB-114: a panic while the store's write lock is held must not poison it.
/// `std::sync::RwLock` poisons on exactly this, after which every later
/// `.read()`/`.write()` panics too — the server looks dead until restart.
mod lock_poisoning {
    use super::*;
    use horndb_sparql::algebra::TriplePattern;
    use horndb_sparql::exec::mem::MemStore;
    use horndb_sparql::exec::{AlgebraQuad, AlgebraTriple, ApplyCounts, Bindings, Executor};

    /// Wraps `MemStore`, panicking on the first `apply_quads` call (the
    /// SPARQL Update write path) and delegating normally after. Reads
    /// delegate straight through, unaffected.
    struct PanicOnceStore {
        inner: MemStore,
        panicked: bool,
    }

    impl Executor for PanicOnceStore {
        fn scan_bgp(
            &self,
            patterns: &[TriplePattern],
            scope: &ScanScope<'_>,
        ) -> horndb_sparql::Result<Box<dyn Iterator<Item = Bindings> + '_>> {
            self.inner.scan_bgp(patterns, scope)
        }
    }

    impl Store for PanicOnceStore {
        fn apply_quads(
            &mut self,
            dels: Vec<AlgebraQuad>,
            adds: Vec<AlgebraQuad>,
        ) -> horndb_sparql::Result<ApplyCounts> {
            if !self.panicked {
                self.panicked = true;
                panic!("injected update-path panic (HDB-114 test)");
            }
            self.inner.apply_quads(dels, adds)
        }
        fn clear_graph(
            &mut self,
            graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<usize> {
            self.inner.clear_graph(graph)
        }
        fn graph_exists(&self, graph: &str) -> bool {
            self.inner.graph_exists(graph)
        }
        fn graphs(&self) -> Vec<String> {
            self.inner.graphs()
        }
        fn scan_graph_quads(
            &self,
            graph: &spargebra::algebra::GraphTarget,
        ) -> horndb_sparql::Result<Vec<AlgebraTriple>> {
            self.inner.scan_graph_quads(graph)
        }
    }

    #[tokio::test]
    async fn panicking_update_leaves_lock_usable_for_next_request() {
        let state = AppState {
            store: Arc::new(RwLock::new(PanicOnceStore {
                inner: MemStore::default(),
                panicked: false,
            })),
            cfg: SparqlConfig::default(),
            ready: Arc::new(AtomicBool::new(true)),
        };
        let app = build_router(state);

        // First update panics while the write lock is held: the request
        // fails, but must not take the whole server down with it.
        let req = Request::builder()
            .method("POST")
            .uri("/update")
            .header("content-type", "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://ex/x> <http://ex/p> <http://ex/y> }".to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // A later request must answer normally — proof the lock wasn't
        // poisoned by the panic above.
        let req2 = Request::builder()
            .uri("/query?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap();
        let resp2 = app.clone().oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        // And a later update must go through cleanly too (second
        // `apply_quads` call, past the injected panic).
        let req3 = Request::builder()
            .method("POST")
            .uri("/update")
            .header("content-type", "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://ex/x> <http://ex/p> <http://ex/y> }".to_string(),
            ))
            .unwrap();
        let resp3 = app.oneshot(req3).await.unwrap();
        assert_eq!(resp3.status(), StatusCode::NO_CONTENT);
    }
}
