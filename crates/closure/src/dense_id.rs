//! Per-predicate dense renumbering of dictionary IDs.
//!
//! GraphBLAS matrices are most cache-efficient when row/column indices are
//! densely packed starting at 0. The storage crate (SPEC-02) gives us sparse
//! `DictId(u64)` values; we maintain a bijection per predicate so the matrix
//! dimension is exactly the number of distinct subjects/objects appearing in
//! that predicate's extent.
//!
//! Stage-1 simplification: the map is rebuilt from scratch at each bulk
//! materialization checkpoint. SPEC-05 risk note "Dense renumbering
//! invalidation" calls this out — incremental invalidation is Stage 2.

use rustc_hash::FxHashMap;

use crate::types::{DenseIdx, DictId};

/// Bijection `DictId <-> DenseIdx`.
#[derive(Default, Clone)]
pub struct DenseIdMap {
    forward: FxHashMap<DictId, DenseIdx>,
    reverse: Vec<DictId>,
}

impl DenseIdMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            forward: FxHashMap::with_capacity_and_hasher(cap, Default::default()),
            reverse: Vec::with_capacity(cap),
        }
    }

    /// Number of distinct dictionary IDs in the map.
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Insert `id` if not present, return its dense index.
    pub fn intern(&mut self, id: DictId) -> DenseIdx {
        if let Some(&dense) = self.forward.get(&id) {
            return dense;
        }
        let dense = DenseIdx(self.reverse.len() as u64);
        self.reverse.push(id);
        self.forward.insert(id, dense);
        dense
    }

    pub fn to_dense(&self, id: DictId) -> Option<DenseIdx> {
        self.forward.get(&id).copied()
    }

    pub fn to_dict(&self, idx: DenseIdx) -> Option<DictId> {
        self.reverse.get(idx.0 as usize).copied()
    }

    /// Intern both endpoints of every edge and return dense `(u64, u64)` pairs
    /// suitable for `GrB_Matrix_build_BOOL`.
    pub fn intern_edges(&mut self, edges: &[(DictId, DictId)]) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(edges.len());
        for &(s, o) in edges {
            let si = self.intern(s).0;
            let oi = self.intern(o).0;
            out.push((si, oi));
        }
        out
    }
}
