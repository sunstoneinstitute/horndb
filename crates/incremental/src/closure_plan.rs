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

/// A closure operator maintained incrementally (SPEC-06 F5/F6).
///
/// Given the asserted insertion delta for a tick, [`apply_insert_delta`] returns
/// the newly inferred closure triples; given the asserted retraction delta,
/// [`apply_retract_delta`] returns the closure triples to WITHDRAW (the caller
/// negates the sign). Implementations retain their own closed state across
/// calls, so each call only needs this tick's delta edges.
///
/// [`apply_insert_delta`]: ClosureRule::apply_insert_delta
/// [`apply_retract_delta`]: ClosureRule::apply_retract_delta
pub trait ClosureRule {
    fn apply_insert_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId>;

    /// Apply the retraction half of a tick's asserted delta: filter for the
    /// withdrawn (`mult < 0`) base edges this rule owns, update the retained
    /// closure, and return the closure triples that lost ALL support and must
    /// therefore be **withdrawn** (the caller negates the sign).
    ///
    /// Default impl returns `Vec::new()` so an insertion-only closure rule
    /// still compiles and never withdraws anything. `TransitiveClosureRule`
    /// overrides it.
    fn apply_retract_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId> {
        let _ = asserted_delta;
        Vec::new()
    }
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
///
/// For a warm store that already holds edges for this predicate, call
/// `seed_closed_edges` with the predicate's materialized (already-closed)
/// extent before registering the rule, so incremental inserts close against
/// the pre-existing reachable state.
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

    /// Seed the retained closure from an **already transitively-closed** edge
    /// set for this predicate (e.g. a warm store's materialized closure)
    /// before feeding incremental inserts. Edges are `(s, o)` dictionary-id
    /// pairs and MUST already be closed — this does not re-close them.
    ///
    /// Call this once, after `new` and before the rule is registered on a
    /// `Circuit` that already holds edges for this predicate; otherwise the
    /// first incremental inserts would not see the pre-existing reachable
    /// state and would miss the transitive edges that bridge old and new
    /// edges (SPEC-06 acceptance #1, warm-store case).
    pub fn seed_closed_edges(&mut self, closed_edges: &[(u64, u64)]) {
        let edges: Vec<(DictId, DictId)> = closed_edges
            .iter()
            .map(|&(s, o)| (DictId(s), DictId(o)))
            .collect();
        self.backend
            .seed_transitive_closure(PredicateId(self.predicate), &edges);
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

    fn apply_retract_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId> {
        // Collect negative-multiplicity edges for this predicate (the base
        // edges being withdrawn this tick).
        let edges: Vec<(DictId, DictId)> = asserted_delta
            .iter()
            .filter(|((_, p, _), mult)| *p == self.predicate && *mult < 0)
            .map(|((s, _, o), _)| (DictId(*s), DictId(*o)))
            .collect();
        if edges.is_empty() {
            return Vec::new();
        }
        let pid = PredicateId(self.predicate);
        // Mirror `apply_insert_delta`: a GraphBLAS-level failure is not
        // recoverable here, so surface it as a panic.
        let withdrawn = self
            .backend
            .delete_transitive_edges(pid, &edges)
            .expect("incremental closure delete failed");
        withdrawn
            .into_iter()
            .map(|(s, o)| (s.0, self.predicate, o.0))
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

    /// Insertion-only contract: a negative-multiplicity entry contributes no
    /// edge (retraction is F6, deferred). Only the positive edge is closed.
    #[test]
    fn transitive_rule_ignores_negative_multiplicities() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((1, 100, 2), 1);
        delta.add((2, 100, 3), -1); // retraction: ignored at Stage 1
        let got = rule.apply_insert_delta(&delta);
        // Only the positive (1,100,2) edge; no (2,3), no transitive (1,3).
        assert_eq!(got, vec![(1, 100, 2)]);
    }

    /// Warm-store path: seeding the rule with an already-closed edge set lets a
    /// later insert produce the cross-product transitive edges against the
    /// pre-existing reachable state (codex review P2 / SPEC-06 acceptance #1).
    #[test]
    fn transitive_rule_seeded_warm_store_closes_against_existing() {
        let mut rule = TransitiveClosureRule::new(100);
        // Already-closed extent for p=100: 1->2, 2->3, and the transitive 1->3.
        rule.seed_closed_edges(&[(1, 2), (2, 3), (1, 3)]);
        // Insert (3,4): must close to (3,4) plus (1,4),(2,4) via the seeded state.
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((3, 100, 4), 1);
        let mut got = rule.apply_insert_delta(&delta);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 100, 4), (2, 100, 4), (3, 100, 4)]);
    }

    /// Retracting a base edge in the middle of a chain withdraws the closure
    /// pairs that lose all support. p=100, chain 1->2->3: retracting (2,p,3)
    /// withdraws the transitive (1,p,3) and the direct (2,p,3); (1,p,2)
    /// survives.
    #[test]
    fn transitive_rule_retract_chain_withdraws_unsupported() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut ins: Zset<crate::types::TripleId> = Zset::new();
        ins.add((1, 100, 2), 1);
        ins.add((2, 100, 3), 1);
        let _ = rule.apply_insert_delta(&ins);

        let mut del: Zset<crate::types::TripleId> = Zset::new();
        del.add((2, 100, 3), -1);
        let mut got = rule.apply_retract_delta(&del);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 100, 3), (2, 100, 3)]);
    }

    /// Diamond: 1->2, 1->3, 2->4, 3->4 closes (1,4) via two paths. Retracting
    /// (2,4) does NOT withdraw (1,4) — the 1->3->4 path still supports it.
    #[test]
    fn transitive_rule_retract_keeps_alternately_supported() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut ins: Zset<crate::types::TripleId> = Zset::new();
        ins.add((1, 100, 2), 1);
        ins.add((1, 100, 3), 1);
        ins.add((2, 100, 4), 1);
        ins.add((3, 100, 4), 1);
        let _ = rule.apply_insert_delta(&ins);

        let mut del: Zset<crate::types::TripleId> = Zset::new();
        del.add((2, 100, 4), -1);
        let got = rule.apply_retract_delta(&del);
        // (1,4) is still reachable via 1->3->4, so the only withdrawn pair is
        // the direct (2,4).
        assert_eq!(got, vec![(2, 100, 4)]);
    }

    /// A retraction-only delta with no negative edge for this predicate
    /// withdraws nothing (positives are ignored here — that is the insert path).
    #[test]
    fn transitive_rule_retract_ignores_positives_and_other_predicates() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut ins: Zset<crate::types::TripleId> = Zset::new();
        ins.add((1, 100, 2), 1);
        let _ = rule.apply_insert_delta(&ins);

        let mut del: Zset<crate::types::TripleId> = Zset::new();
        del.add((1, 100, 2), 1); // positive: ignored by retract path
        del.add((1, 999, 2), -1); // other predicate: ignored
        let got = rule.apply_retract_delta(&del);
        assert!(got.is_empty());
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
