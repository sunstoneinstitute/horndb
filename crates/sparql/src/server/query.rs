//! `/query` HTTP handler. Per SPARQL 1.1 Protocol:
//!   * GET with `query` in the URL query string,
//!   * POST `application/sparql-query` raw,
//!   * POST `application/x-www-form-urlencoded` with `query=`.

use super::AppState;
use crate::api::{execute_query, QueryAnswer};
use crate::results::{
    csv::write_select_csv, json::write_ask_json, json::write_select_json, tsv::write_select_tsv,
    xml::write_ask_xml, xml::write_select_xml, ResultFormat,
};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct QueryParams {
    pub query: Option<String>,
}

pub async fn handle_query_get(
    State(state): State<AppState>,
    Query(p): Query<QueryParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(q) = p.query else {
        return (
            StatusCode::BAD_REQUEST,
            "missing `query` parameter".to_string(),
        )
            .into_response();
    };
    run(state, &q, &headers).await
}

pub async fn handle_query_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Per the protocol, `application/x-www-form-urlencoded` carries
    // a `query=` field; `application/sparql-query` is raw. We sniff.
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let query = if ctype.contains("application/x-www-form-urlencoded") {
        match url_form_query(&body) {
            Some(q) => q,
            None => {
                return (StatusCode::BAD_REQUEST, "form missing `query`".to_string())
                    .into_response();
            }
        }
    } else {
        body
    };
    run(state, &query, &headers).await
}

fn url_form_query(body: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "query" {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal decoder — sufficient for tests. `urlencoding` crate
    // would be the prod choice; avoid the dep in Stage 1.
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn run(state: AppState, q: &str, headers: &HeaderMap) -> axum::response::Response {
    // Scope the read guard to the execution only; results are
    // materialised into `ans`, so serialization below holds no lock and
    // never blocks a concurrent writer.
    let ans = {
        let store = state.store.read().unwrap();
        match execute_query(q, &*store) {
            Ok(a) => a,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
        }
    };

    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let fmt = ResultFormat::from_accept(accept);

    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            let body = match fmt {
                ResultFormat::Json => write_select_json(&vars, &rows),
                ResultFormat::Xml => write_select_xml(&vars, &rows),
                ResultFormat::Csv => write_select_csv(&vars, &rows),
                ResultFormat::Tsv => write_select_tsv(&vars, &rows),
            };
            (StatusCode::OK, [("content-type", fmt.content_type())], body).into_response()
        }
        QueryAnswer::Boolean(b) => {
            // CSV/TSV have no boolean serialisation; fall back to XML
            // (the protocol default for ASK in many clients) for those.
            let (ctype, body) = match fmt {
                ResultFormat::Json => (ResultFormat::Json.content_type(), write_ask_json(b)),
                _ => (ResultFormat::Xml.content_type(), write_ask_xml(b)),
            };
            (StatusCode::OK, [("content-type", ctype)], body).into_response()
        }
        QueryAnswer::Triples(triples) => {
            // Stage 1: serialise CONSTRUCT as N-Triples.
            let mut s = String::new();
            for (sub, p, o) in triples {
                s.push_str(&format!("<{sub}> <{p}> <{o}> .\n"));
            }
            (
                StatusCode::OK,
                [("content-type", "application/n-triples")],
                s,
            )
                .into_response()
        }
    }
}

/// Pulled out for re-use by `/update`'s form-encoded body path.
pub(crate) fn url_form_update(body: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "update" {
                return Some(percent_decode(v));
            }
        }
    }
    None
}
