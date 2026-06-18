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
        self.inner
            .lock()
            .expect("audit-log mutex poisoned")
            .push(entry);
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
    pub fn query_since(&self, since: DateTime<Utc>, offset: usize, limit: usize) -> AuditPage {
        let guard = self.inner.lock().expect("audit-log mutex poisoned");
        // Paginate *during* iteration so we only clone the requested page,
        // not the whole filtered log. `offset`/`limit` are caller-controlled
        // HTTP params, so this bounds the per-request work (and the time the
        // mutex is held) to `limit` clones regardless of log size.
        //
        // We still iterate past the page by one matching entry to learn
        // whether `next_offset` should be set, but we never *clone* beyond
        // the page.
        let mut matched = 0usize; // count of entries matching `since`
        let mut entries: Vec<MlAuditEntry> = Vec::new();
        let mut more = false;
        for e in guard.iter().filter(|e| e.timestamp >= since) {
            if matched < offset {
                matched += 1;
                continue;
            }
            if entries.len() < limit {
                entries.push(e.clone());
            } else {
                // One matching entry beyond the page exists.
                more = true;
                break;
            }
            matched += 1;
        }
        // `next_offset` is the index (into the filtered stream) the caller
        // should resume from. `saturating_add` guards a near-`usize::MAX`
        // offset from overflowing.
        let next_offset = if more {
            Some(offset.saturating_add(entries.len()))
        } else {
            None
        };
        AuditPage {
            entries,
            next_offset,
        }
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
    fn huge_offset_does_not_overflow() {
        // Regression: `offset + limit` near usize::MAX must not panic
        // (debug) or wrap (release). Saturating add + min keeps it safe.
        let log = MlAuditLog::new();
        log.record(make_entry(Utc::now(), "m"));
        let p = log.query_since(Utc::now() - chrono::Duration::hours(1), usize::MAX - 1, 10);
        assert!(p.entries.is_empty());
        assert!(p.next_offset.is_none());

        // Also a large-but-in-range-ish offset with max limit.
        let p2 = log.query_since(Utc::now() - chrono::Duration::hours(1), 0, usize::MAX);
        assert_eq!(p2.entries.len(), 1);
        assert!(p2.next_offset.is_none());
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
