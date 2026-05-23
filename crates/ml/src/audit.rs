//! Audit log for ML-derived facts (SPEC-08 F6, library form).
//!
//! Stage 0/1 exposes only the in-memory log + paginated query as a
//! library API. The HTTP `GET /ml-audit?since=` endpoint is Stage 2
//! and will simply wrap this type.

use crate::types::{Confidence, ModelId, TripleSubject};
use chrono::{DateTime, Utc};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct MlAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub model: ModelId,
    pub confidence: Confidence,
    /// `(subject, predicate_iri, object_subject_or_literal)` — kept
    /// loose at Stage 1 since SPEC-02 hasn't fixed its term model yet.
    pub triple: (TripleSubject, String, TripleSubject),
}

#[derive(Debug, Clone)]
pub struct AuditPage {
    pub entries: Vec<MlAuditEntry>,
    /// Token to pass to the next call to continue paginating. `None`
    /// means no more entries.
    pub next_offset: Option<usize>,
}

#[derive(Debug, Default)]
pub struct MlAuditLog {
    inner: Mutex<Vec<MlAuditEntry>>,
}

impl MlAuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, entry: MlAuditEntry) {
        self.inner.lock().expect("audit-log mutex poisoned").push(entry);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("audit-log mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return entries with timestamp >= `since`, paginated.
    ///
    /// `offset` is the index into the filtered result, not the raw
    /// log — so a caller can keep paginating with the returned token
    /// even as new entries arrive.
    pub fn query_since(
        &self,
        since: DateTime<Utc>,
        offset: usize,
        limit: usize,
    ) -> AuditPage {
        let guard = self.inner.lock().expect("audit-log mutex poisoned");
        let filtered: Vec<MlAuditEntry> = guard
            .iter()
            .filter(|e| e.timestamp >= since)
            .cloned()
            .collect();
        let end = (offset + limit).min(filtered.len());
        let entries = if offset >= filtered.len() {
            Vec::new()
        } else {
            filtered[offset..end].to_vec()
        };
        let next_offset = if end < filtered.len() { Some(end) } else { None };
        AuditPage { entries, next_offset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(ts: DateTime<Utc>, model: &str) -> MlAuditEntry {
        MlAuditEntry {
            timestamp: ts,
            model: ModelId::new(model),
            confidence: Confidence::new(0.9),
            triple: (
                TripleSubject::Iri("http://x/a".into()),
                "http://www.w3.org/2002/07/owl#sameAs".into(),
                TripleSubject::Iri("http://x/b".into()),
            ),
        }
    }

    #[test]
    fn empty_log_returns_empty_page() {
        let log = MlAuditLog::new();
        let p = log.query_since(Utc::now() - chrono::Duration::hours(1), 0, 10);
        assert!(p.entries.is_empty());
        assert!(p.next_offset.is_none());
    }

    #[test]
    fn record_then_query() {
        let log = MlAuditLog::new();
        let t = Utc::now();
        log.record(make_entry(t, "m1"));
        let p = log.query_since(t - chrono::Duration::seconds(1), 0, 10);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].model.as_str(), "m1");
    }

    #[test]
    fn since_filter_excludes_older() {
        let log = MlAuditLog::new();
        let old = Utc::now() - chrono::Duration::hours(2);
        let new = Utc::now();
        log.record(make_entry(old, "old"));
        log.record(make_entry(new, "new"));
        let p = log.query_since(Utc::now() - chrono::Duration::hours(1), 0, 10);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].model.as_str(), "new");
    }

    #[test]
    fn pagination_returns_next_offset_when_more_available() {
        let log = MlAuditLog::new();
        let base = Utc::now();
        for i in 0..5 {
            log.record(make_entry(base + chrono::Duration::seconds(i), "m"));
        }
        let p1 = log.query_since(base - chrono::Duration::seconds(1), 0, 2);
        assert_eq!(p1.entries.len(), 2);
        assert_eq!(p1.next_offset, Some(2));

        let p2 = log.query_since(base - chrono::Duration::seconds(1), 2, 2);
        assert_eq!(p2.entries.len(), 2);
        assert_eq!(p2.next_offset, Some(4));

        let p3 = log.query_since(base - chrono::Duration::seconds(1), 4, 2);
        assert_eq!(p3.entries.len(), 1);
        assert_eq!(p3.next_offset, None);
    }
}
