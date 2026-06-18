//! Integration tests for the ML HTTP boundary (SPEC-08 F3 + F6).
//!
//! These drive the axum router directly via `tower::ServiceExt::oneshot`
//! — no socket, no live LLM. The translator is the deterministic
//! `MockTranslator` and the SPARQL executor is a fake, so the suite is
//! fully hermetic (the SPEC-08 requirement: the engine bundles no model).

#![cfg(feature = "server")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_ml::audit::MlAuditEntry;
use horndb_ml::config::{LlmPrivacy, MlConfig};
use horndb_ml::nlquery::{
    CostReport, MockTranslator, NlQuestion, SparqlExecutor, TranslateError, Translation, Translator,
};
use horndb_ml::registry::MlRegistry;
use horndb_ml::server::{build_router, MlAppState};
use horndb_ml::types::{Confidence, ModelId, TripleSubject};
use tower::ServiceExt; // for `oneshot`

/// A fake executor that echoes back the SPARQL it was handed (so tests
/// can assert the boundary forwarded the generated query), or fails.
struct EchoExecutor {
    fail: bool,
}
impl SparqlExecutor for EchoExecutor {
    fn execute(&self, sparql: &str, _accept: &str) -> Result<String, String> {
        if self.fail {
            Err("executor: boom".to_string())
        } else {
            Ok(format!("{{\"ran\":{sparql:?}}}"))
        }
    }
}

/// An adversarial translator that echoes the raw question in its
/// `explanation` — modelling a third-party translator that does not honour
/// the question-free convention. The endpoint must enforce privacy itself.
struct LeakyTranslator;
impl Translator for LeakyTranslator {
    fn model_id(&self) -> ModelId {
        ModelId::new("leaky")
    }
    fn translate(&self, q: &NlQuestion) -> Result<Translation, TranslateError> {
        Ok(Translation {
            generated_sparql: "SELECT * WHERE { ?s ?p ?o }".to_string(),
            confidence: Confidence::new(0.5),
            explanation: format!("you asked: {}", q.question),
            model: ModelId::new("leaky"),
            cost: CostReport::zero(),
        })
    }
}

fn state_with(registry: MlRegistry, fail_exec: bool) -> MlAppState {
    MlAppState::new(
        Arc::new(registry),
        Arc::new(EchoExecutor { fail: fail_exec }),
    )
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn post_nl(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/nl-query")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn nl_query_translates_and_executes() {
    let reg = MlRegistry::new(MlConfig::enabled());
    reg.register_translator(Arc::new(MockTranslator::new(
        "mock-v1",
        "SELECT * WHERE { ?s ?p ?o } LIMIT 1",
    )));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "list everything"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;

    // Generated SPARQL is always returned (audit-critical).
    assert_eq!(v["generated_sparql"], "SELECT * WHERE { ?s ?p ?o } LIMIT 1");
    assert_eq!(v["executed"], true);
    // Executor echoed the SPARQL back inside results.
    assert!(v["results"].as_str().unwrap().contains("SELECT * WHERE"));
    assert_eq!(v["model"], "mock-v1");
    // Cost reporting present.
    assert!(v["cost"]["total_tokens"].as_u64().unwrap() > 0);
    assert!(v["cost"]["estimated_usd"].as_f64().unwrap() >= 0.0);
}

#[tokio::test]
async fn nl_query_dry_run_skips_execution_but_returns_sparql() {
    let reg = MlRegistry::new(MlConfig::enabled());
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "ASK { ?s ?p ?o }")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(
            serde_json::json!({"question": "any data?", "dry_run": true}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["generated_sparql"], "ASK { ?s ?p ?o }");
    assert_eq!(v["executed"], false);
    assert!(v["results"].is_null());
}

#[tokio::test]
async fn nl_query_execution_error_still_returns_generated_sparql() {
    let reg = MlRegistry::new(MlConfig::enabled());
    reg.register_translator(Arc::new(MockTranslator::new(
        "mock-v1",
        "SELECT * WHERE {}",
    )));
    let app = build_router(state_with(reg, true)); // executor fails

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "x"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    // Critical: SPARQL is still returned so the user can correct it.
    assert_eq!(v["generated_sparql"], "SELECT * WHERE {}");
    assert_eq!(v["executed"], true);
    assert_eq!(v["execution_error"], "executor: boom");
    assert!(v["results"].is_null());
}

#[tokio::test]
async fn nl_query_fails_closed_when_ml_disabled() {
    // Disabled registry -> disabled translator -> 503, no guessing.
    let reg = MlRegistry::new(MlConfig::disabled());
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "SELECT * {}")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "x"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn nl_query_empty_question_is_400() {
    let reg = MlRegistry::new(MlConfig::enabled());
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "SELECT * {}")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "   "})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn nl_query_respects_privacy_no_retention_by_default() {
    let reg = MlRegistry::new(MlConfig::enabled());
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "SELECT * {}")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(
            serde_json::json!({"question": "secret PII question"}),
        ))
        .await
        .unwrap();
    let v = body_json(resp).await;
    // Default privacy retains nothing — question_log omitted entirely.
    assert!(v.get("question_log").is_none());
    // And the raw question must not leak through any other field
    // (e.g. the translator's explanation).
    assert!(
        !v.to_string().contains("secret PII question"),
        "raw question leaked in response: {v}"
    );
}

#[tokio::test]
async fn nl_query_retains_question_when_policy_allows() {
    let reg = MlRegistry::new(MlConfig::enabled().with_privacy(LlmPrivacy::retain_questions()));
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "SELECT * {}")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "who is alice?"})))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["question_log"], "who is alice?");
}

#[tokio::test]
async fn nl_query_redacts_question_when_policy_redacts() {
    let privacy = LlmPrivacy {
        log_questions: true,
        redact_in_logs: true,
    };
    let reg = MlRegistry::new(MlConfig::enabled().with_privacy(privacy));
    reg.register_translator(Arc::new(MockTranslator::new("mock-v1", "SELECT * {}")));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "abcde"})))
        .await
        .unwrap();
    let v = body_json(resp).await;
    let log = v["question_log"].as_str().unwrap();
    assert_eq!(log, "[redacted: 5 chars]");
    assert!(!log.contains("abcde"));
}

#[tokio::test]
async fn nl_query_suppresses_leaky_explanation_under_no_retention() {
    // A translator that echoes the question in `explanation` must not leak
    // it: under no-retention the endpoint suppresses the explanation field.
    let reg = MlRegistry::new(MlConfig::enabled()); // default = no retention
    reg.register_translator(Arc::new(LeakyTranslator));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(
            serde_json::json!({"question": "secret PII question"}),
        ))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert!(
        v.get("explanation").is_none(),
        "explanation should be suppressed"
    );
    assert!(
        !v.to_string().contains("secret PII question"),
        "raw question leaked via explanation: {v}"
    );
    // Structured fields are unaffected.
    assert_eq!(v["generated_sparql"], "SELECT * WHERE { ?s ?p ?o }");
}

#[tokio::test]
async fn nl_query_returns_explanation_when_retention_allowed() {
    let reg = MlRegistry::new(MlConfig::enabled().with_privacy(LlmPrivacy::retain_questions()));
    reg.register_translator(Arc::new(LeakyTranslator));
    let app = build_router(state_with(reg, false));

    let resp = app
        .oneshot(post_nl(serde_json::json!({"question": "who is alice?"})))
        .await
        .unwrap();
    let v = body_json(resp).await;
    // Operator opted into retention — explanation is passed through.
    assert_eq!(v["explanation"], "you asked: who is alice?");
}

// ---------- F6: /ml-audit ----------

fn seed_audit(reg: &MlRegistry, n: usize) {
    let log = reg.audit_log();
    let base = chrono::Utc::now();
    for i in 0..n {
        log.record(MlAuditEntry {
            timestamp: base + chrono::Duration::seconds(i as i64),
            model: ModelId::new("faiss-v1"),
            confidence: Confidence::new(0.9),
            triple: (
                TripleSubject::Iri(format!("http://x/s{i}")),
                "http://www.w3.org/2002/07/owl#sameAs".to_string(),
                TripleSubject::Iri(format!("http://x/o{i}")),
            ),
        });
    }
}

fn get_audit(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn ml_audit_fails_closed_when_ml_disabled() {
    // Even with seeded facts, a disabled registry must not serve the audit
    // log — the whole ML HTTP surface is opt-in / fail-closed.
    let reg = MlRegistry::new(MlConfig::enabled());
    seed_audit(&reg, 3);
    reg.reload_config(MlConfig::disabled());
    let app = build_router(state_with(reg, false));
    let resp = app.oneshot(get_audit("/ml-audit")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn ml_audit_returns_seeded_facts() {
    let reg = MlRegistry::new(MlConfig::enabled());
    seed_audit(&reg, 3);
    let app = build_router(state_with(reg, false));

    let resp = app.oneshot(get_audit("/ml-audit")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["model"], "faiss-v1");
    assert_eq!(entries[0]["confidence"], 0.9);
    assert!(entries[0]["predicate"].as_str().unwrap().contains("sameAs"));
}

#[tokio::test]
async fn ml_audit_paginates() {
    let reg = MlRegistry::new(MlConfig::enabled());
    seed_audit(&reg, 5);
    let app = build_router(state_with(reg, false));

    let resp = app
        .clone()
        .oneshot(get_audit("/ml-audit?limit=2"))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["entries"].as_array().unwrap().len(), 2);
    assert_eq!(v["next_offset"], 2);

    let resp = app
        .oneshot(get_audit("/ml-audit?limit=2&offset=4"))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert!(v.get("next_offset").is_none());
}

#[tokio::test]
async fn ml_audit_rejects_bad_since() {
    let reg = MlRegistry::new(MlConfig::enabled());
    let app = build_router(state_with(reg, false));
    let resp = app
        .oneshot(get_audit("/ml-audit?since=not-a-timestamp"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ml_audit_since_filters() {
    let reg = MlRegistry::new(MlConfig::enabled());
    let log = reg.audit_log();
    let old = chrono::Utc::now() - chrono::Duration::hours(2);
    let recent = chrono::Utc::now();
    for (ts, m) in [(old, "old"), (recent, "new")] {
        log.record(MlAuditEntry {
            timestamp: ts,
            model: ModelId::new(m),
            confidence: Confidence::new(0.5),
            triple: (
                TripleSubject::Iri("http://x/a".into()),
                "http://x/p".into(),
                TripleSubject::Iri("http://x/b".into()),
            ),
        });
    }
    let app = build_router(state_with(reg, false));
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let resp = app
        .oneshot(get_audit(&format!("/ml-audit?since={cutoff}")))
        .await
        .unwrap();
    let v = body_json(resp).await;
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["model"], "new");
}
