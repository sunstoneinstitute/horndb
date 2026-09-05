//! SPEC-28 S5 — the Graph Store Protocol routes on `/graphs`.
//!
//! The `?default`-on-a-materialized-store refusal lives in its own test
//! binary (`graph_store_materialized.rs`): the flag is a one-way
//! process-global, so setting it here would leak into every other test in
//! this file.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::server::{build_router, AppState, Limits};
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const G: &str = "http://ex/g";

fn router() -> axum::Router {
    router_with_body_cap(4 * 1024 * 1024)
}

fn router_with_body_cap(max_request_body: usize) -> axum::Router {
    build_router(AppState {
        store: Arc::new(RwLock::new(MemStore::default())),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Limits::new(4, Duration::from_secs(5), max_request_body),
    })
}

fn uri(graph: &str) -> String {
    format!("/graphs?graph={}", urlencode(graph))
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

struct Resp {
    status: StatusCode,
    content_type: String,
    body: String,
}

async fn send(app: &axum::Router, method: &str, uri: &str, ctype: &str, body: &str) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    if !ctype.is_empty() {
        req = req.header(
            if method == "GET" {
                "accept"
            } else {
                "content-type"
            },
            ctype,
        );
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    Resp {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn put(app: &axum::Router, uri: &str, body: &str) -> Resp {
    send(app, "PUT", uri, "text/turtle", body).await
}

async fn get(app: &axum::Router, uri: &str, accept: &str) -> Resp {
    send(app, "GET", uri, accept, "").await
}

const TRIPLE_A: &str = "<http://ex/a> <http://ex/p> <http://ex/o> .";
const TRIPLE_B: &str = "<http://ex/b> <http://ex/p> <http://ex/o> .";

// --- the status-code table ------------------------------------------------

#[tokio::test]
async fn put_creates_then_replaces_then_get_and_delete() {
    let app = router();

    // 404 before anything exists.
    assert_eq!(get(&app, &uri(G), "").await.status, StatusCode::NOT_FOUND);

    // 201: the graph held no visible quads before the write.
    assert_eq!(
        put(&app, &uri(G), TRIPLE_A).await.status,
        StatusCode::CREATED
    );

    let r = get(&app, &uri(G), "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "text/turtle");
    assert!(r.body.contains("http://ex/a"), "{}", r.body);

    // 204: replaced. The old triple is gone, the new one is there — the
    // whole point of PUT being a replace, not a merge.
    assert_eq!(
        put(&app, &uri(G), TRIPLE_B).await.status,
        StatusCode::NO_CONTENT
    );
    let r = get(&app, &uri(G), "application/n-triples").await;
    assert_eq!(r.content_type, "application/n-triples");
    assert!(r.body.contains("http://ex/b"), "{}", r.body);
    assert!(!r.body.contains("http://ex/a"), "{}", r.body);

    // 204: deleted, then 404 for both GET and the idempotent second DELETE.
    assert_eq!(
        send(&app, "DELETE", &uri(G), "", "").await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get(&app, &uri(G), "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        send(&app, "DELETE", &uri(G), "", "").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn post_merges_rather_than_replaces() {
    let app = router();
    // 201 on the empty graph.
    assert_eq!(
        send(&app, "POST", &uri(G), "text/turtle", TRIPLE_A)
            .await
            .status,
        StatusCode::CREATED
    );
    // 204 once it exists — and A survives.
    assert_eq!(
        send(&app, "POST", &uri(G), "text/turtle", TRIPLE_B)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let r = get(&app, &uri(G), "application/n-triples").await;
    assert!(
        r.body.contains("http://ex/a") && r.body.contains("http://ex/b"),
        "{}",
        r.body
    );

    // Re-POSTing what is already there is an idempotent no-op: 204.
    assert_eq!(
        send(&app, "POST", &uri(G), "text/turtle", TRIPLE_A)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn default_graph_is_served_not_refused() {
    let app = router();
    assert_eq!(
        put(&app, "/graphs?default", TRIPLE_A).await.status,
        StatusCode::CREATED
    );
    let r = get(&app, "/graphs?default", "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("http://ex/a"), "{}", r.body);
    // The write landed in the default graph, not a named one.
    assert_eq!(get(&app, &uri(G), "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        send(&app, "DELETE", "/graphs?default", "", "").await.status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn bad_requests() {
    let app = router();

    // Neither `graph` nor `default`.
    assert_eq!(
        get(&app, "/graphs", "").await.status,
        StatusCode::BAD_REQUEST
    );
    // An unknown query parameter.
    let r = get(&app, "/graphs?graph=http%3A%2F%2Fex%2Fg&branch=main", "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("branch"), "{}", r.body);
    // Both.
    assert_eq!(
        get(&app, "/graphs?default&graph=http%3A%2F%2Fex%2Fg", "")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );

    // A parse error carries the parser's own message.
    let r = put(&app, &uri(G), "<http://ex/a> <http://ex/p> ;;; .").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.body.contains("parsing"),
        "parser message expected: {}",
        r.body
    );
}

#[tokio::test]
async fn dataset_media_types_are_415() {
    let app = router();
    for ctype in [
        "application/trig",
        "application/n-quads",
        "application/json",
    ] {
        let r = send(&app, "PUT", &uri(G), ctype, "").await;
        assert_eq!(
            r.status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{ctype} must be refused"
        );
    }
}

#[tokio::test]
async fn oversized_payload_is_413() {
    let app = router_with_body_cap(64);
    let big = (0..50)
        .map(|i| format!("<http://ex/s{i}> <http://ex/p> <http://ex/o> .\n"))
        .collect::<String>();
    assert_eq!(
        put(&app, &uri(G), &big).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

// --- the four behaviours SPEC-28 S5 is emphatic about ---------------------

/// S5: an empty diff commits nothing and returns 204. Observed here as the
/// status plus an unchanged graph; the "commits nothing" half is pinned by
/// `server::graph_store::tests::identical_payload_diffs_to_nothing`.
#[tokio::test]
async fn reput_of_an_identical_body_is_an_empty_diff() {
    let app = router();
    let body = format!("{TRIPLE_A}\n{TRIPLE_B}\n");
    assert_eq!(put(&app, &uri(G), &body).await.status, StatusCode::CREATED);
    let before = get(&app, &uri(G), "application/n-triples").await.body;
    assert_eq!(
        put(&app, &uri(G), &body).await.status,
        StatusCode::NO_CONTENT
    );
    let after = get(&app, &uri(G), "application/n-triples").await.body;
    assert_eq!(before, after);
}

/// S5: blank nodes are request-scoped, so re-`PUT`ting an identical
/// bnode-bearing body is *not* an empty diff — every bnode-touching quad is
/// deleted and re-inserted under a fresh label. Asserted rather than assumed,
/// because it is the one case where PUT is not idempotent.
#[tokio::test]
async fn bnode_payloads_are_request_scoped_so_reput_replaces() {
    let app = router();
    let body = "_:x <http://ex/p> <http://ex/o> .";
    assert_eq!(put(&app, &uri(G), body).await.status, StatusCode::CREATED);
    let first = get(&app, &uri(G), "application/n-triples").await.body;
    assert_eq!(
        put(&app, &uri(G), body).await.status,
        StatusCode::NO_CONTENT
    );
    let second = get(&app, &uri(G), "application/n-triples").await.body;

    // Still exactly one quad — the old bnode's quad was deleted, not kept.
    assert_eq!(
        second.lines().filter(|l| !l.is_empty()).count(),
        1,
        "{second}"
    );
    // Under a different label: the request-scoped bnode never equalled the
    // stored one.
    assert_ne!(first, second, "the bnode label must not survive the re-PUT");
}

/// S5: reserved graphs are read-only over GSP — `GET` allowed, the three
/// write verbs refused with the namespace named in the body.
#[tokio::test]
async fn reserved_graphs_are_read_only() {
    let app = router();
    let reserved = uri("https://horndb.io/graph/inferred");

    for (method, ctype) in [
        ("PUT", "text/turtle"),
        ("POST", "text/turtle"),
        ("DELETE", ""),
    ] {
        let r = send(&app, method, &reserved, ctype, TRIPLE_A).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{method}");
        assert!(
            r.body.contains("https://horndb.io/graph/"),
            "{method} must name the namespace: {}",
            r.body
        );
    }

    // A read is allowed — it just finds nothing here.
    assert_eq!(get(&app, &reserved, "").await.status, StatusCode::NOT_FOUND);
}
