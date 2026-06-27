//! GraphBLAS-backed [`ClosureBackend`] (SPEC-05, #61).
//!
//! Drop-in replacement for [`crate::backend::RuleFiringBackend`] that computes
//! the transitive-closure-shaped rules (`scm-sco`, `scm-spo`, `eq-sym`,
//! `eq-trans`, `prp-trp`) via SuiteSparse:GraphBLAS sparse-matrix closure
//! instead of nested-loop rule firing.
//!
//! # Parity
//!
//! This backend MUST return the same triple **set** as `RuleFiringBackend` for
//! every input — that is the acceptance gate for #61, enforced by
//! `tests/closure_backend_differential.rs`. The non-obvious parity decisions:
//!
//! - `scm-sco` / `scm-spo` use the **strict** transitive closure
//!   ([`transitive_closure`], no identity). `RuleFiringBackend` does not add
//!   reflexive `?c ⊑ ?c` — that comes from a separate compiled rule, not the
//!   backend. We therefore deliberately do **not** use
//!   `horndb_closure::sink::BackendImpl`, whose `close_subclass` adds the
//!   identity (`reflexive_transitive_closure`).
//! - `owl:sameAs` is closed as the strict transitive closure of the
//!   **symmetrized** edge set `M ∨ Mᵀ`. This reproduces `eq-sym` followed by
//!   `eq-trans` iterated to fixpoint, including the diagonal `(a,a)` for any
//!   element in a class of size ≥ 2 (`a↔b ⇒ a→b→a`). We emit the closure as
//!   triples (the engine materializes `eq-*` into the store) rather than into a
//!   `EquivClasses` union-find, which is why we bypass `BackendImpl::add_sameas`.
//! - `eq-ref` (reflexive `?x sameAs ?x` for arbitrary `x`) is **not** computed,
//!   matching `RuleFiringBackend`.
//!
//! Provenance `premises` are recorded best-effort (empty): the gate is the
//! derived triple set, not the proof tree. `rule_id` is set correctly so
//! downstream provenance routing (SPEC-08) still sees the right rule.

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::dense_id::DenseIdMap;
use horndb_closure::grb::{init_once, BoolMatrix};
use horndb_closure::types::{DenseIdx, DictId};
use rustc_hash::FxHashSet;

use crate::backend::ClosureBackend;
use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{RuleId, TermId, Triple};

/// SPEC-05 GraphBLAS closure backend. Stateless beyond the one-time GraphBLAS
/// initialization performed in [`GraphBlasBackend::new`].
pub struct GraphBlasBackend;

impl GraphBlasBackend {
    pub fn new() -> Self {
        // Idempotent; cheap to call repeatedly. Panics only if GraphBLAS itself
        // fails to initialize, which is unrecoverable for this backend.
        init_once().expect("GraphBLAS GrB_init failed");
        Self
    }
}

impl Default for GraphBlasBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureBackend for GraphBlasBackend {
    fn close(&mut self, store: &dyn TripleStore) -> Delta {
        let v = *store.vocab();
        let mut out = Delta::new();

        // scm-sco: strict transitive closure of rdfs:subClassOf.
        close_transitive(store, v.rdfs_sub_class_of, "scm-sco", &mut out);
        // scm-spo: strict transitive closure of rdfs:subPropertyOf.
        close_transitive(store, v.rdfs_sub_property_of, "scm-spo", &mut out);
        // eq-sym + eq-trans: strict transitive closure of the symmetrized
        // owl:sameAs relation.
        close_sameas(store, v.owl_same_as, &mut out);
        // prp-trp: strict transitive closure of each declared transitive property.
        close_transitive_properties(store, &v, &mut out);
        // TODO(TASKS.md #130): SPEC-11 T1 — this backend does not yet close the
        // SSSOM mapping predicates (skos:exactMatch/broadMatch/narrowMatch) that
        // RuleFiringBackend closes. Bring to parity (SPEC-11/SPEC-05 follow-up).

        out
    }
}

/// Read predicate `pred`'s edges from the store as dictionary-id pairs.
fn scan_edges(store: &dyn TripleStore, pred: TermId) -> Vec<(DictId, DictId)> {
    store
        .scan_predicate(pred)
        .map(|t| (DictId(t.s.0), DictId(t.o.0)))
        .collect()
}

/// Strict transitive closure of `edges` over a dense renumbering. Returns the
/// closure edges mapped back to `TermId` pairs. Empty input → empty output.
fn closure_edges(edges: &[(DictId, DictId)]) -> Vec<(TermId, TermId)> {
    if edges.is_empty() {
        return Vec::new();
    }
    let mut map = DenseIdMap::with_capacity(edges.len() * 2);
    let dense = map.intern_edges(edges);
    let n = map.len() as u64;
    let matrix = BoolMatrix::from_edges(n, &dense).expect("BoolMatrix::from_edges");
    let closed = transitive_closure(&matrix).expect("transitive_closure");
    let closed_edges = closed.extract_edges().expect("extract_edges");
    closed_edges
        .iter()
        .filter_map(|&(s, o)| {
            let s_dict = map.to_dict(DenseIdx(s))?;
            let o_dict = map.to_dict(DenseIdx(o))?;
            Some((TermId(s_dict.0), TermId(o_dict.0)))
        })
        .collect()
}

/// Emit the strict transitive closure of `pred` under `rule_id`, skipping
/// triples already present in the store.
fn close_transitive(store: &dyn TripleStore, pred: TermId, rule_id: RuleId, out: &mut Delta) {
    let edges = scan_edges(store, pred);
    for (s, o) in closure_edges(&edges) {
        emit(store, Triple::new(s, pred, o), rule_id, out);
    }
}

/// Emit the symmetric-transitive closure of `owl:sameAs`. Edges whose reverse
/// is an asserted `sameAs` pair are attributed to `eq-sym`; the rest to
/// `eq-trans` (best-effort — only the triple set is gated).
fn close_sameas(store: &dyn TripleStore, same_as: TermId, out: &mut Delta) {
    let asserted = scan_edges(store, same_as);
    if asserted.is_empty() {
        return;
    }
    let asserted_set: FxHashSet<(DictId, DictId)> = asserted.iter().copied().collect();
    // Symmetrize before closing: M ∨ Mᵀ.
    let mut sym = Vec::with_capacity(asserted.len() * 2);
    for &(a, b) in &asserted {
        sym.push((a, b));
        sym.push((b, a));
    }
    for (s, o) in closure_edges(&sym) {
        // A derived edge that is the reverse of an asserted pair is an eq-sym
        // step; anything else only arises through transitivity.
        let rule_id: RuleId = if asserted_set.contains(&(DictId(o.0), DictId(s.0))) {
            "eq-sym"
        } else {
            "eq-trans"
        };
        emit(store, Triple::new(s, same_as, o), rule_id, out);
    }
}

/// Close every predicate declared `(p rdf:type owl:TransitiveProperty)`.
fn close_transitive_properties(
    store: &dyn TripleStore,
    vocab: &crate::vocab::Vocabulary,
    out: &mut Delta,
) {
    let predicates: Vec<TermId> = store
        .scan_predicate(vocab.rdf_type)
        .filter(|t| t.o == vocab.owl_transitive_property)
        .map(|t| t.s)
        .collect();
    for p in predicates {
        close_transitive(store, p, "prp-trp", out);
    }
}

/// Insert `head` into `out` iff it is not already materialized in the store.
fn emit(store: &dyn TripleStore, head: Triple, rule_id: RuleId, out: &mut Delta) {
    if !store.contains(&head) {
        out.insert(head, Provenance::new(rule_id, std::iter::empty()));
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
    fn subclass_chain_closes_strictly() {
        let v = Vocabulary::synthetic(1000);
        let sco = v.rdfs_sub_class_of;
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C ⊑ D
        store.assert(t(1, sco.0, 2));
        store.assert(t(2, sco.0, 3));
        store.assert(t(3, sco.0, 4));
        let delta = GraphBlasBackend::new().close(&store);
        assert!(delta.contains(&t(1, sco.0, 3)));
        assert!(delta.contains(&t(2, sco.0, 4)));
        assert!(delta.contains(&t(1, sco.0, 4)));
        // Strict: no reflexive self-edges from the backend.
        assert!(!delta.contains(&t(1, sco.0, 1)));
        assert!(!delta.contains(&t(4, sco.0, 4)));
    }

    #[test]
    fn sameas_symmetric_and_transitive() {
        let v = Vocabulary::synthetic(1000);
        let sa = v.owl_same_as;
        let mut store = MemStore::new(v);
        store.assert(t(1, sa.0, 2));
        store.assert(t(2, sa.0, 3));
        let delta = GraphBlasBackend::new().close(&store);
        // symmetry
        assert!(delta.contains(&t(2, sa.0, 1)));
        assert!(delta.contains(&t(3, sa.0, 2)));
        // transitivity
        assert!(delta.contains(&t(1, sa.0, 3)));
        assert!(delta.contains(&t(3, sa.0, 1)));
        // diagonal for a non-singleton class (a↔b ⇒ a→a)
        assert!(delta.contains(&t(1, sa.0, 1)));
    }

    #[test]
    fn transitive_property_closes() {
        let v = Vocabulary::synthetic(1000);
        let (ty, tp) = (v.rdf_type, v.owl_transitive_property);
        let p = TermId(500);
        let mut store = MemStore::new(v);
        store.assert(t(p.0, ty.0, tp.0)); // p is a TransitiveProperty
        store.assert(t(1, p.0, 2));
        store.assert(t(2, p.0, 3));
        let delta = GraphBlasBackend::new().close(&store);
        assert!(delta.contains(&t(1, p.0, 3)), "prp-trp");
    }

    #[test]
    fn empty_store_is_noop() {
        let v = Vocabulary::synthetic(1000);
        let store = MemStore::new(v);
        let delta = GraphBlasBackend::new().close(&store);
        assert!(delta.is_empty());
    }
}
