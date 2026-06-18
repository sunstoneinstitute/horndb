//! Natural-language → SPARQL translation boundary (SPEC-08 F3).
//!
//! The principle (SPEC-00 / SPEC-08): the symbolic engine is the source
//! of truth. The LLM only *translates* a natural-language question into
//! SPARQL; the generated SPARQL is **always** returned so a human can
//! audit it, and execution is delegated to SPEC-07. The engine does not
//! bundle a model — the actual LLM call lives behind the [`Translator`]
//! trait, so tests use a deterministic mock and `cargo test` stays
//! hermetic (no network, no live model).
//!
//! Cost transparency (SPEC-08 "Cost transparency" risk): every
//! translation reports token counts and an estimated USD cost so callers
//! can budget. Training-data leakage controls (SPEC-08 "Training-data
//! leakage" risk) live in [`crate::config::LlmPrivacy`]: the question is
//! only retained/forwarded for logging when the deployment opts in.

use crate::types::{Confidence, ModelId};

/// Token-accounting + cost estimate for one LLM translation call.
///
/// `prompt_tokens` / `completion_tokens` come from the upstream provider
/// (or the mock). `estimated_usd` is computed by the [`Translator`] from
/// its own price card — the engine does not assume a price.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "server", derive(serde::Serialize))]
pub struct CostReport {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub estimated_usd: f64,
}

impl CostReport {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// A zero-cost report — used when no model ran (e.g. the disabled
    /// translator) so the response shape is still well-formed.
    pub fn zero() -> Self {
        CostReport {
            prompt_tokens: 0,
            completion_tokens: 0,
            estimated_usd: 0.0,
        }
    }
}

/// A natural-language question plus optional steering context.
#[derive(Debug, Clone)]
pub struct NlQuestion {
    pub question: String,
    /// Optional schema/ontology hint passed to the model.
    pub schema_hint: Option<String>,
    /// Optional explicit model selection; `None` = translator default.
    pub model_id: Option<String>,
}

/// The result of translating a question to SPARQL.
///
/// `generated_sparql` is **always** populated, even when `confidence` is
/// low — the caller must be able to see and correct what the model
/// produced (SPEC-08 F3, "LLM SPARQL quality" risk).
#[derive(Debug, Clone)]
pub struct Translation {
    pub generated_sparql: String,
    pub confidence: Confidence,
    pub explanation: String,
    pub model: ModelId,
    pub cost: CostReport,
}

/// Errors a translator may surface.
#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    /// The upstream model was unreachable or returned an error.
    #[error("llm translation failed: {0}")]
    Upstream(String),
    /// The model produced no usable SPARQL.
    #[error("llm produced no parseable SPARQL")]
    Empty,
}

/// Translates a natural-language question to SPARQL.
///
/// Implementations wrap a concrete LLM provider (OpenAI, Anthropic, a
/// local model, …). The engine never bundles one; production deployments
/// supply an implementation that calls their chosen API. Tests use
/// [`MockTranslator`].
pub trait Translator: Send + Sync {
    fn model_id(&self) -> ModelId;

    /// Translate `q` into SPARQL. Implementations MUST NOT execute the
    /// query — they only produce the text. The boundary executes it
    /// (via [`SparqlExecutor`]) after the caller has had the chance to
    /// audit it.
    fn translate(&self, q: &NlQuestion) -> Result<Translation, TranslateError>;
}

/// Executes generated SPARQL against the symbolic engine (SPEC-07).
///
/// Kept as a trait so `horndb-ml` does not take a hard dependency on the
/// full SPARQL/storage stack — the boundary is wired to a real executor
/// at the call site (the `serve` binary), and tests inject a fake.
pub trait SparqlExecutor: Send + Sync {
    /// Run `sparql` and return a serialized result document (the caller
    /// chooses the media type via `accept`). On error return a message;
    /// the boundary surfaces it without discarding `generated_sparql`.
    fn execute(&self, sparql: &str, accept: &str) -> Result<String, String>;
}

/// Disabled translator: never produces SPARQL.
///
/// Installed when ML is disabled so the `/nl-query` endpoint degrades to
/// an explicit 503-style "translation unavailable" rather than silently
/// guessing. Keeps NF1 (disabling ML changes nothing about correctness).
#[derive(Debug, Default)]
pub struct DisabledTranslator;

impl DisabledTranslator {
    pub const MODEL_ID: &'static str = "disabled-translator";
}

impl Translator for DisabledTranslator {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }
    fn translate(&self, _q: &NlQuestion) -> Result<Translation, TranslateError> {
        Err(TranslateError::Upstream(
            "ML disabled: no translator configured".to_string(),
        ))
    }
}

/// A deterministic, offline translator for tests and demos.
///
/// It does **no** real NLP: it maps a question to a canned SPARQL string
/// (either a fixed default or a per-question lookup) and reports a fixed
/// token cost. Its entire purpose is to keep the HTTP boundary testable
/// without a live model — exactly the hermeticity SPEC-08 requires.
pub struct MockTranslator {
    model: ModelId,
    default_sparql: String,
    confidence: Confidence,
    /// Per-question canned answers; falls back to `default_sparql`.
    canned: std::collections::HashMap<String, String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    usd_per_1k_tokens: f64,
}

impl MockTranslator {
    pub fn new(model: impl Into<String>, default_sparql: impl Into<String>) -> Self {
        MockTranslator {
            model: ModelId::new(model),
            default_sparql: default_sparql.into(),
            confidence: Confidence::new(0.8),
            canned: std::collections::HashMap::new(),
            prompt_tokens: 16,
            completion_tokens: 24,
            usd_per_1k_tokens: 0.002,
        }
    }

    pub fn with_canned(mut self, question: impl Into<String>, sparql: impl Into<String>) -> Self {
        self.canned.insert(question.into(), sparql.into());
        self
    }

    pub fn with_confidence(mut self, c: Confidence) -> Self {
        self.confidence = c;
        self
    }

    pub fn with_pricing(mut self, usd_per_1k_tokens: f64) -> Self {
        self.usd_per_1k_tokens = usd_per_1k_tokens;
        self
    }

    fn cost(&self) -> CostReport {
        let total = (self.prompt_tokens + self.completion_tokens) as f64;
        CostReport {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            estimated_usd: (total / 1000.0) * self.usd_per_1k_tokens,
        }
    }
}

impl Translator for MockTranslator {
    fn model_id(&self) -> ModelId {
        self.model.clone()
    }

    fn translate(&self, q: &NlQuestion) -> Result<Translation, TranslateError> {
        let sparql = self
            .canned
            .get(&q.question)
            .cloned()
            .unwrap_or_else(|| self.default_sparql.clone());
        if sparql.trim().is_empty() {
            return Err(TranslateError::Empty);
        }
        Ok(Translation {
            generated_sparql: sparql,
            confidence: self.confidence,
            explanation: format!("mock translation of {:?}", q.question),
            model: self.model.clone(),
            cost: self.cost(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> NlQuestion {
        NlQuestion {
            question: s.to_string(),
            schema_hint: None,
            model_id: None,
        }
    }

    #[test]
    fn cost_report_totals_and_zero() {
        let c = CostReport {
            prompt_tokens: 10,
            completion_tokens: 5,
            estimated_usd: 0.03,
        };
        assert_eq!(c.total_tokens(), 15);
        assert_eq!(CostReport::zero().total_tokens(), 0);
        assert_eq!(CostReport::zero().estimated_usd, 0.0);
    }

    #[test]
    fn disabled_translator_errors() {
        let t = DisabledTranslator;
        assert_eq!(t.model_id().as_str(), "disabled-translator");
        assert!(matches!(
            t.translate(&q("anything")),
            Err(TranslateError::Upstream(_))
        ));
    }

    #[test]
    fn mock_returns_default_sparql_and_cost() {
        let t = MockTranslator::new("mock-v1", "SELECT * WHERE { ?s ?p ?o } LIMIT 1");
        let out = t.translate(&q("list everything")).unwrap();
        assert_eq!(out.generated_sparql, "SELECT * WHERE { ?s ?p ?o } LIMIT 1");
        assert_eq!(out.model.as_str(), "mock-v1");
        assert!(out.cost.total_tokens() > 0);
        // 40 tokens * 0.002 / 1000 = 0.00008
        assert!((out.cost.estimated_usd - 0.00008).abs() < 1e-9);
    }

    #[test]
    fn mock_canned_overrides_default() {
        let t = MockTranslator::new("mock-v1", "DEFAULT")
            .with_canned("who is alice", "SELECT ?p WHERE { <alice> ?p ?o }");
        assert_eq!(
            t.translate(&q("who is alice")).unwrap().generated_sparql,
            "SELECT ?p WHERE { <alice> ?p ?o }"
        );
        assert_eq!(
            t.translate(&q("other")).unwrap().generated_sparql,
            "DEFAULT"
        );
    }

    #[test]
    fn empty_sparql_is_rejected() {
        let t = MockTranslator::new("mock-v1", "   ");
        assert!(matches!(t.translate(&q("x")), Err(TranslateError::Empty)));
    }

    #[test]
    fn traits_are_object_safe() {
        let _: Box<dyn Translator> = Box::new(DisabledTranslator);
        struct FakeExec;
        impl SparqlExecutor for FakeExec {
            fn execute(&self, _s: &str, _a: &str) -> Result<String, String> {
                Ok("{}".to_string())
            }
        }
        let _: Box<dyn SparqlExecutor> = Box::new(FakeExec);
    }
}
