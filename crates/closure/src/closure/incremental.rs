//! Insertion-only incremental transitive closure (SPEC-05 F6).
//!
//! The initial bulk closure is computed on GraphBLAS (`closure/transitive.rs`).
//! Once a relation is transitively closed, inserting a single edge `(s, o)`
//! adds exactly the cross product of everything that reaches `s` (inclusive)
//! with everything reachable from `o` (inclusive) — a rank-1 outer-product OR.
//! That is an inherently sparse pointwise update, so we maintain it directly in
//! Rust (forward + backward adjacency) rather than paying GraphBLAS `mxm` cost
//! per edge. Deletion is **not** handled here — it needs SPEC-06 DBSP deltas.

use rustc_hash::{FxHashMap, FxHashSet};

/// Strict transitive closure over dense `u64` indices, maintained incrementally
/// under edge insertion. "Strict" = no implicit identity; a self-loop `(x,x)`
/// appears only when `x` lies on a cycle, matching
/// [`crate::closure::transitive::transitive_closure`].
#[derive(Default, Clone)]
pub struct IncrementalTransitiveClosure {
    fwd: FxHashMap<u64, FxHashSet<u64>>,
    bwd: FxHashMap<u64, FxHashSet<u64>>,
    nnz: usize,
}

impl IncrementalTransitiveClosure {
    /// Empty closure.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from a set of edges that is **already transitively closed** (e.g.
    /// the output of [`crate::closure::transitive::transitive_closure`]). The
    /// caller guarantees closure; this constructor does not re-close.
    pub fn from_closed_edges<I: IntoIterator<Item = (u64, u64)>>(edges: I) -> Self {
        let mut c = Self::default();
        for (s, o) in edges {
            if c.fwd.entry(s).or_default().insert(o) {
                c.bwd.entry(o).or_default().insert(s);
                c.nnz += 1;
            }
        }
        c
    }

    /// Number of edges (`nnz`) currently in the closure.
    pub fn nnz(&self) -> usize {
        self.nnz
    }

    pub fn is_empty(&self) -> bool {
        self.nnz == 0
    }

    /// All closure edges as `(s, o)` pairs (unordered; caller sorts if needed).
    pub fn edges(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(self.nnz);
        for (&s, os) in &self.fwd {
            for &o in os {
                out.push((s, o));
            }
        }
        out
    }

    /// Insert one edge and return the **newly inferred** closure edges (the
    /// delta), i.e. pairs not already present. Maintains the closed invariant.
    pub fn insert_edge(&mut self, s: u64, o: u64) -> Vec<(u64, u64)> {
        // B = {x : x reaches s} ∪ {s}; F = {y : o reaches y} ∪ {o}.
        let mut b: Vec<u64> = self
            .bwd
            .get(&s)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        b.push(s);
        let mut f: Vec<u64> = self
            .fwd
            .get(&o)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        f.push(o);

        let mut delta = Vec::new();
        for &x in &b {
            for &y in &f {
                if self.fwd.entry(x).or_default().insert(y) {
                    self.bwd.entry(y).or_default().insert(x);
                    self.nnz += 1;
                    delta.push((x, y));
                }
            }
        }
        delta
    }

    /// Undo a set of edges that were just inserted by this same logical
    /// operation (transaction rollback — NOT general deletion). Each `(x, y)`
    /// must be an edge that was genuinely added (e.g. the delta returned by
    /// `insert_edge` / `insert_edges`); removing it restores the prior closed
    /// state exactly.
    pub fn rollback_inserted(&mut self, edges: &[(u64, u64)]) {
        for &(x, y) in edges {
            let removed = self
                .fwd
                .get_mut(&x)
                .map(|set| set.remove(&y))
                .unwrap_or(false);
            if removed {
                if let Some(set) = self.bwd.get_mut(&y) {
                    set.remove(&x);
                }
                self.nnz -= 1;
            }
        }
    }

    /// Insert many edges (folded one at a time so later edges observe earlier
    /// contributions) and return the combined delta across all of them.
    pub fn insert_edges<I: IntoIterator<Item = (u64, u64)>>(
        &mut self,
        edges: I,
    ) -> Vec<(u64, u64)> {
        let mut delta = Vec::new();
        for (s, o) in edges {
            delta.extend(self.insert_edge(s, o));
        }
        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge_set(c: &IncrementalTransitiveClosure) -> std::collections::BTreeSet<(u64, u64)> {
        c.edges().into_iter().collect()
    }

    #[test]
    fn empty_has_no_edges() {
        let c = IncrementalTransitiveClosure::default();
        assert_eq!(c.nnz(), 0);
        assert!(c.edges().is_empty());
    }

    #[test]
    fn single_edge_insert_returns_itself() {
        let mut c = IncrementalTransitiveClosure::default();
        let delta = c.insert_edge(1, 2);
        assert_eq!(delta, vec![(1, 2)]);
        assert_eq!(c.nnz(), 1);
    }

    #[test]
    fn reinserting_existing_edge_yields_empty_delta() {
        let mut c = IncrementalTransitiveClosure::default();
        c.insert_edge(1, 2);
        let delta = c.insert_edge(1, 2);
        assert!(delta.is_empty());
        assert_eq!(c.nnz(), 1);
    }

    #[test]
    fn chain_insert_transitively_closes() {
        // 1->2 then 2->3 then 3->4, inserted in order.
        let mut c = IncrementalTransitiveClosure::default();
        c.insert_edge(1, 2);
        c.insert_edge(2, 3);
        let mut delta = c.insert_edge(3, 4);
        delta.sort_unstable();
        // Adding 3->4 to a closure already containing 1->{2,3},2->3 must add
        // (3,4),(2,4),(1,4): B = bwd[3]∪{3} = {1,2,3}, F = fwd[4]∪{4} = {4}.
        assert_eq!(delta, vec![(1, 4), (2, 4), (3, 4)]);
        assert_eq!(
            edge_set(&c),
            [(1, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 4)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn closing_a_cycle_creates_self_loops() {
        // 1->2->3 then 3->1 closes the cycle; strict closure includes the
        // diagonal for every node on the cycle.
        let mut c = IncrementalTransitiveClosure::default();
        c.insert_edge(1, 2);
        c.insert_edge(2, 3);
        c.insert_edge(3, 1);
        assert_eq!(
            edge_set(&c),
            [
                (1, 1),
                (1, 2),
                (1, 3),
                (2, 1),
                (2, 2),
                (2, 3),
                (3, 1),
                (3, 2),
                (3, 3),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn rollback_inserted_restores_prior_state() {
        // Build a chain 1->2->3.
        let mut c = IncrementalTransitiveClosure::default();
        c.insert_edge(1, 2);
        c.insert_edge(2, 3);
        // Capture state before inserting 3->4.
        let pre_nnz = c.nnz();
        let pre_edges = edge_set(&c);

        // Insert 3->4 and immediately roll it back.
        let delta = c.insert_edge(3, 4);
        c.rollback_inserted(&delta);

        assert_eq!(c.nnz(), pre_nnz, "nnz must be restored after rollback");
        assert_eq!(
            edge_set(&c),
            pre_edges,
            "edge set must match pre-insert state"
        );
    }

    #[test]
    fn from_closed_seeds_existing_state() {
        // Seed with an already-closed 1->2->3 (so 1->3 present), then extend.
        let seed = [(1, 2), (1, 3), (2, 3)];
        let mut c = IncrementalTransitiveClosure::from_closed_edges(seed.iter().copied());
        assert_eq!(c.nnz(), 3);
        let mut delta = c.insert_edge(3, 4);
        delta.sort_unstable();
        assert_eq!(delta, vec![(1, 4), (2, 4), (3, 4)]);
    }
}
