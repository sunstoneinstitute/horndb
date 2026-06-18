//! `GET /ml-audit` handler (SPEC-08 F6).
//!
//! Query params: `since` (RFC 3339 timestamp; default: epoch),
//! `offset` (default 0), `limit` (default 100, capped at 1000).
//!
//! Returns every ML-derived fact admitted in the window with its source
//! model id and confidence — enough to drive a human-review UI (F6 /
//! acceptance #4). Wraps the in-process [`MlAuditLog`](crate::audit::MlAuditLog).

use super::MlAppState;
use crate::audit::MlAuditEntry;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct AuditParams {
    /// RFC 3339 timestamp; only facts at or after this are returned.
    pub since: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AuditEntryJson {
    pub timestamp: String,
    pub model: String,
    pub confidence: f64,
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub entries: Vec<AuditEntryJson>,
    /// Pagination token to pass as `offset` on the next call; absent when
    /// no more entries remain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn subject_str(s: &crate::types::TripleSubject) -> String {
    match s {
        crate::types::TripleSubject::Iri(i) => i.clone(),
        crate::types::TripleSubject::BlankNode(b) => format!("_:{b}"),
    }
}

fn to_json(e: &MlAuditEntry) -> AuditEntryJson {
    AuditEntryJson {
        timestamp: e.timestamp.to_rfc3339(),
        model: e.model.as_str().to_string(),
        confidence: e.confidence.value(),
        subject: subject_str(&e.triple.0),
        predicate: e.triple.1.clone(),
        object: subject_str(&e.triple.2),
    }
}

pub async fn handle_ml_audit(
    State(state): State<MlAppState>,
    Query(p): Query<AuditParams>,
) -> impl IntoResponse {
    let since: DateTime<Utc> = match &p.since {
        None => DateTime::<Utc>::UNIX_EPOCH,
        Some(s) => {
            // A `+` in the RFC 3339 offset (e.g. `+00:00`) is decoded to a
            // space by `application/x-www-form-urlencoded` query parsing.
            // RFC 3339 never contains a literal space, so restoring it is
            // unambiguous and saves callers from having to percent-encode.
            let normalized = s.replace(' ', "+");
            match DateTime::parse_from_rfc3339(&normalized) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorBody {
                            error: format!("invalid `since` timestamp (RFC 3339 expected): {e}"),
                        }),
                    )
                        .into_response();
                }
            }
        }
    };

    let offset = p.offset.unwrap_or(0);
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let page = state.registry.audit_log().query_since(since, offset, limit);
    let body = AuditResponse {
        entries: page.entries.iter().map(to_json).collect(),
        next_offset: page.next_offset,
    };
    (StatusCode::OK, Json(body)).into_response()
}
