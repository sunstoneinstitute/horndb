//! Per-request id for access-log correlation (HDB-124).
//!
//! Passes through a caller-supplied `x-request-id` header; otherwise
//! generates one from the process id plus a monotonic per-process counter.
//! No `uuid`/`rand` dependency: a pid+counter pair is unique for the life of
//! the process, which is all a log-correlation id needs — the audit finding
//! is "a slow query can be matched to a client", not global uniqueness.

use axum::http::HeaderMap;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return the incoming `x-request-id` header value if present and
/// non-empty, else `<pid>-<seq>` from a process-wide monotonic counter.
pub fn request_id(headers: &HeaderMap) -> String {
    if let Some(v) = headers.get("x-request-id").and_then(|v| v.to_str().ok()) {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{seq}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_caller_supplied_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "client-abc-123".parse().unwrap());
        assert_eq!(request_id(&headers), "client-abc-123");
    }

    #[test]
    fn generates_distinct_ids_when_absent() {
        let headers = HeaderMap::new();
        let a = request_id(&headers);
        let b = request_id(&headers);
        assert_ne!(a, b, "successive generated ids must differ: {a} vs {b}");
    }

    #[test]
    fn empty_header_falls_back_to_generated() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "".parse().unwrap());
        assert!(!request_id(&headers).is_empty());
    }
}
