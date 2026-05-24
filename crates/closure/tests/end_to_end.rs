use std::sync::Mutex;

use reasoner_closure::sink::{ClosureBackend, TripleSink};
use reasoner_closure::types::{DictId, PredicateId, Triple};

/// A `TripleSink` that just accumulates into a Vec. Used by tests until the
/// storage crate provides a real implementation.
#[derive(Default)]
struct VecSink {
    triples: Mutex<Vec<Triple>>,
}

impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> Result<u64, anyhow::Error> {
        let mut guard = self.triples.lock().unwrap();
        let before = guard.len();
        guard.extend(triples);
        Ok((guard.len() - before) as u64)
    }
}

#[test]
fn transitive_predicate_closes_and_writes_back() {
    let sink = VecSink::default();
    let mut backend = reasoner_closure::sink::default_backend();

    // Predicate p = 42; transitive chain 1->2->3->4.
    let p = PredicateId(42);
    let edges = vec![
        (DictId(1), DictId(2)),
        (DictId(2), DictId(3)),
        (DictId(3), DictId(4)),
    ];

    let written = backend
        .close_transitive_predicate(p, &edges, &sink)
        .expect("close transitive predicate");

    // Asserted = 3, closure adds (1,3),(1,4),(2,4) = 3 new. Writeback inserts
    // the *full* closure (the backend does not yet diff against asserted —
    // the storage layer is responsible for de-duping on bulk insert).
    assert_eq!(written, 6);

    let triples = sink.triples.lock().unwrap();
    assert_eq!(triples.len(), 6);
    let pairs: Vec<(u64, u64)> = triples.iter().map(|t| (t.s.0, t.o.0)).collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    let expected: Vec<(u64, u64)> = vec![
        (1, 2), (1, 3), (1, 4),
        (2, 3), (2, 4),
        (3, 4),
    ];
    assert_eq!(sorted, expected);
    for t in triples.iter() {
        assert_eq!(t.p, p);
    }
}
