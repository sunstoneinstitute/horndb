//! GraphBLAS-backed [`ClosureBackend`] (SPEC-05, #61).
//!
//! Drop-in replacement for [`crate::backend::RuleFiringBackend`] that computes
//! the transitive-closure-shaped rules (`scm-sco`, `scm-spo`, `eq-sym`,
//! `eq-trans`, `prp-trp`) via SuiteSparse:GraphBLAS sparse-matrix closure
//! instead of nested-loop rule firing.
//!
//! The rule shapes themselves live in [`crate::closure_shape`], which also
//! documents the parity decisions this backend is gated on. This file only
//! supplies the closure kernel.

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::dense_id::DenseIdMap;
use horndb_closure::grb::{init_once, BoolMatrix};
use horndb_closure::types::{DenseIdx, DictId};

use crate::backend::ClosureBackend;
use crate::closure_shape::{self, ClosureKernel};
use crate::delta::Delta;
use crate::store::TripleStore;
use crate::types::TermId;

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
        closure_shape::close_all(store, self)
    }
}

impl ClosureKernel for GraphBlasBackend {
    fn closure_edges(&mut self, edges: &[(DictId, DictId)]) -> Vec<(TermId, TermId)> {
        closure_edges(edges)
    }
}

/// Strict transitive closure of `edges` over a dense renumbering. Returns the
/// closure edges mapped back to `TermId` pairs. Empty input -> empty output.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use crate::types::Triple;
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
