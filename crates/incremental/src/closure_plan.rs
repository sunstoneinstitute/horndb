//! SPEC-06 F5 — closure-operator delta plans.
//!
//! A `ClosureRule` consumes the asserted insertion delta for a tick and
//! returns the newly inferred closure triples (insertion-only). The concrete
//! `TransitiveClosureRule` wraps SPEC-05's `IncrementalClosureBackend` for one
//! transitive predicate, mapping `TripleId` ⇄ closure dictionary IDs and
//! collecting the delta triples the backend writes.

use std::sync::Mutex;

use horndb_closure::sink::{IncrementalClosureBackend, TripleSink};
use horndb_closure::types::{DictId, PredicateId, Triple};

use crate::types::TripleId;
use crate::zset::Zset;

/// A closure operator maintained incrementally under insertions (SPEC-06 F5).
///
/// Given the asserted insertion delta for a tick, returns the newly inferred
/// closure triples. Implementations retain their own closed state across
/// calls, so each call only needs this tick's new edges. Insertion-only:
/// negative multiplicities are ignored (retraction is F6, deferred).
pub trait ClosureRule {
    fn apply_insert_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId>;
}

/// Collects the delta triples written by `IncrementalClosureBackend`.
///
/// `TripleSink` requires `Sync`, so we use a `Mutex` rather than a `RefCell`.
/// The sink is short-lived: created per `apply_insert_delta` call, drained
/// immediately after.
#[derive(Default)]
struct VecSink {
    collected: Mutex<Vec<Triple>>,
}

impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> anyhow::Result<u64> {
        let mut guard = self.collected.lock().expect("VecSink lock poisoned");
        let before = guard.len();
        guard.extend(triples);
        Ok((guard.len() - before) as u64)
    }
}

/// Incremental transitive closure for a single predicate `p`, wrapping
/// SPEC-05's `IncrementalClosureBackend`.
///
/// `p` is the predicate component of the `TripleId`s this rule handles; only
/// asserted-delta triples whose middle component equals `p` and whose
/// multiplicity is positive contribute edges. The backend emits only the
/// newly inferred edges (including a freshly inserted direct edge), so output
/// is already deduplicated against the rule's own retained closure.
pub struct TransitiveClosureRule {
    predicate: u64,
    backend: IncrementalClosureBackend,
}

impl TransitiveClosureRule {
    pub fn new(predicate: u64) -> Self {
        Self {
            predicate,
            backend: IncrementalClosureBackend::new(),
        }
    }
}

impl ClosureRule for TransitiveClosureRule {
    fn apply_insert_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId> {
        // Collect positive-multiplicity edges for this predicate.
        let edges: Vec<(DictId, DictId)> = asserted_delta
            .iter()
            .filter(|((_, p, _), mult)| *p == self.predicate && *mult > 0)
            .map(|((s, _, o), _)| (DictId(*s), DictId(*o)))
            .collect();
        if edges.is_empty() {
            return Vec::new();
        }
        let sink = VecSink::default();
        let pid = PredicateId(self.predicate);
        // The in-memory VecSink never errors; surface a panic if the backend
        // itself does (GraphBLAS-level failure is not recoverable here).
        self.backend
            .insert_transitive_edges(pid, &edges, &sink)
            .expect("incremental closure insert failed");
        let collected = sink.collected.into_inner().expect("VecSink lock poisoned");
        collected
            .into_iter()
            .map(|t| (t.s.0, t.p.0, t.o.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zset::Zset;

    /// p = 100. Inserting the chain (1,p,2),(2,p,3) in one delta yields the
    /// transitive edge (1,p,3) plus the two direct edges as inferred output.
    #[test]
    fn transitive_rule_chain_one_delta() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((1, 100, 2), 1);
        delta.add((2, 100, 3), 1);
        let mut got = rule.apply_insert_delta(&delta);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 100, 2), (1, 100, 3), (2, 100, 3)]);
    }

    /// Edges for other predicates are ignored by a rule bound to p=100.
    #[test]
    fn transitive_rule_ignores_other_predicates() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((1, 100, 2), 1);
        delta.add((1, 999, 2), 1); // different predicate
        let got = rule.apply_insert_delta(&delta);
        assert_eq!(got, vec![(1, 100, 2)]);
    }

    /// State is retained across calls: the second delta sees the first.
    #[test]
    fn transitive_rule_retains_state_across_deltas() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut d1: Zset<crate::types::TripleId> = Zset::new();
        d1.add((1, 100, 2), 1);
        let _ = rule.apply_insert_delta(&d1);

        let mut d2: Zset<crate::types::TripleId> = Zset::new();
        d2.add((2, 100, 3), 1);
        let mut got = rule.apply_insert_delta(&d2);
        got.sort_unstable();
        // Only the *new* edges: (2,3) direct and (1,3) transitive. (1,2) was
        // already emitted in the first delta and is not re-emitted.
        assert_eq!(got, vec![(1, 100, 3), (2, 100, 3)]);
    }
}
