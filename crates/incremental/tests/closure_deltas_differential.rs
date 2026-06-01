//! SPEC-06 F5 differential test: the Circuit closure path produces the same
//! transitive closure as a full from-scratch recompute (SPEC-05 `BackendImpl`),
//! for an arbitrary sequence of edge insertions split across arbitrary tick
//! boundaries. Insertion-only (SPEC-06 acceptance #4 shape, closure subset).

use std::collections::BTreeSet;
use std::sync::Mutex;

use horndb_closure::sink::{BackendImpl, ClosureBackend, TripleSink};
use horndb_closure::types::{DictId, PredicateId, Triple};
use horndb_incremental::{Circuit, TransitiveClosureRule};
use proptest::prelude::*;

const P: u64 = 7;

/// Collecting sink for the oracle.
#[derive(Default)]
struct VecSink {
    collected: Mutex<Vec<Triple>>,
}
impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> anyhow::Result<u64> {
        let mut g = self.collected.lock().unwrap();
        let before = g.len();
        g.extend(triples);
        Ok((g.len() - before) as u64)
    }
}

/// Full closure of `edges` under predicate P, as a set of (s,o) pairs.
fn oracle_closure(edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    if edges.is_empty() {
        return BTreeSet::new();
    }
    let mut backend = BackendImpl::default();
    let sink = VecSink::default();
    let dict_edges: Vec<(DictId, DictId)> =
        edges.iter().map(|&(s, o)| (DictId(s), DictId(o))).collect();
    backend
        .close_transitive_predicate(PredicateId(P), &dict_edges, &sink)
        .unwrap();
    sink.collected
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|t| (t.s.0, t.o.0))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For a random edge list and random tick split points, the Circuit's
    /// (asserted ∪ derived) support for predicate P equals the full closure.
    #[test]
    fn circuit_closure_matches_full_recompute(
        edges in prop::collection::vec((0u64..8, 0u64..8), 0..24),
        tick_every in 1usize..=4,
    ) {
        let mut c = Circuit::new();
        c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

        for (i, &(s, o)) in edges.iter().enumerate() {
            c.assert_triple((s, P, o));
            if (i + 1) % tick_every == 0 {
                c.tick();
            }
        }
        c.tick(); // flush any remaining

        let mut got: BTreeSet<(u64, u64)> = BTreeSet::new();
        for ((s, p, o), m) in c.asserted_base().iter() {
            if *p == P && m > 0 {
                got.insert((*s, *o));
            }
        }
        for ((s, p, o), m) in c.derived_base().iter() {
            if *p == P && m > 0 {
                got.insert((*s, *o));
            }
        }

        let want = oracle_closure(&edges);
        prop_assert_eq!(got, want);
    }
}
