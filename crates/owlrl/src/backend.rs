//! Closure-backend trait: equality and transitive-property closure.
//!
//! In production, `horndb-closure` (SPEC-05) implements this trait against
//! SuiteSparse:GraphBLAS. In tests and for Stage-1 smoke runs, the
//! `RuleFiringBackend` defined here runs the closure as ordinary rule firing
//! (slow but obviously correct).

use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{TermId, Triple};
use smallvec::smallvec;

/// Compute the closure subset (equality, transitive properties, subClassOf,
/// subPropertyOf transitivity) and return the deltas to insert.
///
/// Implementations may mutate internal caches but MUST NOT mutate the store
/// — the caller owns that. The returned Delta is applied by the engine.
pub trait ClosureBackend {
    /// Compute the full closure given the current store. Called once per
    /// semi-naïve round when any predicate the backend cares about is dirty.
    fn close(&mut self, store: &dyn TripleStore) -> Delta;
}

/// Reference implementation that fires the closure-delegated rules as ordinary
/// nested-loop rules until fixed point. Used by Stage-1 tests; not for
/// production workloads.
pub struct RuleFiringBackend;

impl RuleFiringBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuleFiringBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureBackend for RuleFiringBackend {
    fn close(&mut self, store: &dyn TripleStore) -> Delta {
        let v = *store.vocab();
        let mut out = Delta::new();
        loop {
            let before = out.len();
            // scm-sco: subClassOf transitivity.
            close_transitive(store, v.rdfs_sub_class_of, "scm-sco", &mut out);
            // scm-spo: subPropertyOf transitivity.
            close_transitive(store, v.rdfs_sub_property_of, "scm-spo", &mut out);
            // eq-sym: sameAs symmetry.
            close_symmetric(store, v.owl_same_as, "eq-sym", &mut out);
            // eq-trans: sameAs transitivity.
            close_transitive(store, v.owl_same_as, "eq-trans", &mut out);
            // prp-trp: explicit transitive properties.
            close_transitive_property(store, &v, &mut out);
            if out.len() == before {
                break;
            }
        }
        out
    }
}

/// Helper: chain-close a single predicate (the body `?a p ?b /\ ?b p ?c → ?a p ?c`).
fn close_transitive(store: &dyn TripleStore, pred: TermId, rule_id: &'static str, out: &mut Delta) {
    let edges: Vec<(TermId, TermId)> = store
        .scan_predicate(pred)
        .map(|t| (t.s, t.o))
        .chain(out.triples().filter(|t| t.p == pred).map(|t| (t.s, t.o)))
        .collect();
    for &(a, b) in &edges {
        for &(b2, c) in &edges {
            if b == b2 {
                let head = Triple::new(a, pred, c);
                if !store.contains(&head) && !out.contains(&head) {
                    out.insert(
                        head,
                        Provenance {
                            rule_id,
                            premises: smallvec![Triple::new(a, pred, b), Triple::new(b, pred, c),],
                        },
                    );
                }
            }
        }
    }
}

fn close_symmetric(store: &dyn TripleStore, pred: TermId, rule_id: &'static str, out: &mut Delta) {
    let edges: Vec<(TermId, TermId)> = store
        .scan_predicate(pred)
        .map(|t| (t.s, t.o))
        .chain(out.triples().filter(|t| t.p == pred).map(|t| (t.s, t.o)))
        .collect();
    for &(a, b) in &edges {
        let head = Triple::new(b, pred, a);
        if !store.contains(&head) && !out.contains(&head) {
            out.insert(
                head,
                Provenance {
                    rule_id,
                    premises: smallvec![Triple::new(a, pred, b)],
                },
            );
        }
    }
}

fn close_transitive_property(
    store: &dyn TripleStore,
    vocab: &crate::vocab::Vocabulary,
    out: &mut Delta,
) {
    // Find each predicate p s.t. (p rdf:type owl:TransitiveProperty).
    let trans_pred = vocab.owl_transitive_property;
    let rdf_type = vocab.rdf_type;
    let predicates: Vec<TermId> = store
        .scan_predicate(rdf_type)
        .filter(|t| t.o == trans_pred)
        .map(|t| t.s)
        .collect();
    for p in predicates {
        close_transitive(store, p, "prp-trp", out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn subclass_chain_closes() {
        let v = Vocabulary::synthetic(1000);
        let sco = v.rdfs_sub_class_of;
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C ⊑ D
        store.assert(t(1, sco.0, 2));
        store.assert(t(2, sco.0, 3));
        store.assert(t(3, sco.0, 4));
        let delta = RuleFiringBackend::new().close(&store);
        assert!(delta.contains(&t(1, sco.0, 3)));
        assert!(delta.contains(&t(2, sco.0, 4)));
        assert!(delta.contains(&t(1, sco.0, 4)));
    }

    #[test]
    fn sameas_symmetric_and_transitive() {
        let v = Vocabulary::synthetic(1000);
        let sa = v.owl_same_as;
        let mut store = MemStore::new(v);
        store.assert(t(1, sa.0, 2));
        store.assert(t(2, sa.0, 3));
        let delta = RuleFiringBackend::new().close(&store);
        assert!(delta.contains(&t(2, sa.0, 1)));
        assert!(delta.contains(&t(3, sa.0, 2)));
        assert!(delta.contains(&t(1, sa.0, 3)));
        assert!(delta.contains(&t(3, sa.0, 1)));
    }
}
