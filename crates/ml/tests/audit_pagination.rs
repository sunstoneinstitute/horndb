//! F6: the audit log records ML-derived facts and supports
//! `since`-windowed paginated reads.

use chrono::{Duration, Utc};
use horndb_ml::audit::MlAuditEntry;
use horndb_ml::types::{Confidence, ModelId, TripleSubject};
use horndb_ml::{MlConfig, MlRegistry};
use std::sync::Arc;
use std::thread;

#[test]
fn concurrent_writers_then_paginated_read() {
    let r = Arc::new(MlRegistry::new(MlConfig::enabled()));
    let log = r.audit_log();
    let base = Utc::now();

    let handles: Vec<_> = (0..4u64)
        .map(|tid| {
            let log = log.clone();
            thread::spawn(move || {
                for i in 0..25u64 {
                    log.record(MlAuditEntry {
                        timestamp: base + Duration::milliseconds((tid * 25 + i) as i64),
                        model: ModelId::new(format!("model-{tid}")),
                        confidence: Confidence::new(0.5),
                        triple: (
                            TripleSubject::Iri(format!("http://x/s{i}")),
                            "http://www.w3.org/2002/07/owl#sameAs".into(),
                            TripleSubject::Iri(format!("http://x/o{i}")),
                        ),
                    });
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(log.len(), 100);

    // Paginate from the beginning of the window.
    let since = base - Duration::seconds(1);
    let mut seen = 0usize;
    let mut offset = 0usize;
    loop {
        let page = log.query_since(since, offset, 30);
        seen += page.entries.len();
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(seen, 100);
}
