//! `POST /nl-query` handler (SPEC-08 F3).
//!
//! Request:  `{"question": "...", "schema_hint"?: "...", "model_id"?: "..."}`
//! Response: `{"generated_sparql", "results"?, "confidence", "explanation",
//!            "model", "cost", "executed", "question_log"?}`.
//!
//! Invariants enforced here:
//! * `generated_sparql` is always present on a successful translation,
//!   even if execution fails or is skipped — the user must be able to
//!   audit and correct it (F3, "LLM SPARQL quality" risk).
//! * The raw question is only echoed back / retained when the privacy
//!   policy permits it ([`LlmPrivacy::loggable_text`](crate::config::LlmPrivacy::loggable_text)).
//! * When ML is disabled the registered translator is the `Disabled*`
//!   no-op, which errors — so the endpoint fails closed (503) rather than
//!   guessing.

use super::MlAppState;
use crate::nlquery::{NlQuestion, TranslateError};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct NlQueryRequest {
    pub question: String,
    #[serde(default)]
    pub schema_hint: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    /// Accept media type forwarded to the SPARQL executor. Defaults to
    /// SPARQL JSON results.
    #[serde(default)]
    pub accept: Option<String>,
    /// If true, only translate — do not execute. The generated SPARQL is
    /// still returned so the caller can review before running it.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct CostJson {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub estimated_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct NlQueryResponse {
    /// Always present on success — audit-critical (F3).
    pub generated_sparql: String,
    /// Serialized SPARQL results, or `None` when `dry_run` or execution
    /// failed (`execution_error` is set in the latter case).
    pub results: Option<String>,
    pub confidence: f64,
    pub explanation: String,
    pub model: String,
    pub cost: CostJson,
    /// Whether the generated SPARQL was executed.
    pub executed: bool,
    /// Set when execution was attempted and failed; the generated SPARQL
    /// is still returned for correction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_error: Option<String>,
    /// Echo of the question subject to the privacy policy. `None` when
    /// retention is disabled (the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_log: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

pub async fn handle_nl_query(
    State(state): State<MlAppState>,
    Json(req): Json<NlQueryRequest>,
) -> impl IntoResponse {
    if req.question.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "`question` must not be empty".to_string(),
            }),
        )
            .into_response();
    }

    let translator = state.registry.translator();
    let privacy = state.registry.llm_privacy();
    let question_log = privacy.loggable_text(&req.question);

    let q = NlQuestion {
        question: req.question.clone(),
        schema_hint: req.schema_hint.clone(),
        model_id: req.model_id.clone(),
    };

    let translation = match translator.translate(&q) {
        Ok(t) => t,
        Err(e) => {
            // Disabled/no-op translator surfaces as 503; a genuine empty
            // translation as 422; an upstream failure as 502.
            let status = match &e {
                TranslateError::Upstream(m) if m.contains("ML disabled") => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                TranslateError::Upstream(_) => StatusCode::BAD_GATEWAY,
                TranslateError::Empty => StatusCode::UNPROCESSABLE_ENTITY,
            };
            return (
                status,
                Json(ErrorBody {
                    error: e.to_string(),
                }),
            )
                .into_response();
        }
    };

    let cost = CostJson {
        prompt_tokens: translation.cost.prompt_tokens,
        completion_tokens: translation.cost.completion_tokens,
        total_tokens: translation.cost.total_tokens(),
        estimated_usd: translation.cost.estimated_usd,
    };

    // Execute unless dry_run. The generated SPARQL is returned regardless.
    let (results, executed, execution_error) = if req.dry_run {
        (None, false, None)
    } else {
        let accept = req
            .accept
            .as_deref()
            .unwrap_or("application/sparql-results+json");
        match state
            .executor
            .execute(&translation.generated_sparql, accept)
        {
            Ok(r) => (Some(r), true, None),
            Err(e) => (None, true, Some(e)),
        }
    };

    let body = NlQueryResponse {
        generated_sparql: translation.generated_sparql,
        results,
        confidence: translation.confidence.value(),
        explanation: translation.explanation,
        model: translation.model.as_str().to_string(),
        cost,
        executed,
        execution_error,
        question_log,
    };

    (StatusCode::OK, Json(body)).into_response()
}
