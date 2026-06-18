//! Incremental transitive closure (SPEC-05 F6) — insertion and retraction.
//!
//! The initial bulk closure is computed on GraphBLAS (`closure/transitive.rs`).
//! Once a relation is transitively closed, inserting a single edge `(s, o)`
//! adds exactly the cross product of everything that reaches `s` (inclusive)
//! with everything reachable from `o` (inclusive) — a rank-1 outer-product OR.
//! That is an inherently sparse pointwise update, so we maintain it directly in
//! Rust (forward + backward adjacency) rather than paying GraphBLAS `mxm` cost
//! per edge.
//!
//! Retraction is handled here too (closure-level; SPEC-06 owns the +/- sign).
//! Because a closed pair `(x, y)` may be supported by several distinct base
//! paths, we cannot tell from the closed set alone whether withdrawing a base
//! edge actually removes it. We therefore retain the **asserted base edge set**
//! alongside the closed adjacency. On `delete_edge` we recompute reachability
//! for the affected region against the post-delete base and withdraw only the
//! pairs that are genuinely no longer derivable. This is the DBSP
//! "recompute the affected slice and diff" primitive (SPEC-05 F6: "Deletion
//! uses SPEC-06's DBSP machinery rather than DRed").

use rustc_hash::{FxHashMap, FxHashSet};

/// Outcome of retracting one or more base edges from the closure.
///
/// `withdrawn` are closure pairs that lost ALL support and were dropped from
/// the closed set (the negative delta; the SPEC-06 layer negates them).
///
/// `survived` are base edges that were just deleted this operation but REMAIN
/// reachable in the post-delete closure — i.e. the deleted edge `(s, o)` is
/// still in the closed set because `o` is still reachable from `s` over the
/// remaining base. Only the directly-deleted edge can become such a survivor:
/// transitively-implied pairs were already materialized in the closure
/// (`derived_base`/`closure_support` at the SPEC-06 layer), whereas a deleted
/// *asserted* edge that is still entailed has no materialized derived row, so
/// the SPEC-06 layer must PROMOTE it to one (BUG P1).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub withdrawn: Vec<(u64, u64)>,
    pub survived: Vec<(u64, u64)>,
}

/// Strict transitive closure over dense `u64` indices, maintained incrementally
/// under edge insertion and retraction. "Strict" = no implicit identity; a
/// self-loop `(x,x)` appears only when `x` lies on a cycle, matching
/// [`crate::closure::transitive::transitive_closure`].
///
/// `fwd`/`bwd` hold the **closed** adjacency. `base` holds **only** the
/// asserted (base) edges — the support needed for correct retraction. The
/// closed set always equals `transitive_closure(base)` after any sequence of
/// inserts/deletes, **provided** the instance was built from base edges
/// ([`Self::new`] / [`Self::from_base_edges`] / [`Self::insert_edge`]). An
/// instance seeded via [`Self::from_closed_edges`] has an empty `base` and so
/// **cannot** be retracted correctly — see that constructor's docs.
#[derive(Default, Clone)]
pub struct IncrementalTransitiveClosure {
    fwd: FxHashMap<u64, FxHashSet<u64>>,
    bwd: FxHashMap<u64, FxHashSet<u64>>,
    /// Asserted base edges only (forward adjacency). Empty for instances built
    /// via `from_closed_edges`.
    base: FxHashMap<u64, FxHashSet<u64>>,
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
    ///
    /// **Retraction is unsupported on an instance built this way.** The base
    /// (asserted) edges are unknown — only the closed set is provided — so
    /// [`Self::delete_edge`] cannot tell which closed pairs lose support when a
    /// base edge is withdrawn. The `base` map is left empty, which means every
    /// `delete_edge` call is a no-op (the edge is never found in `base`). Use
    /// [`Self::from_base_edges`] when the instance must support retraction.
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

    /// Seed from a set of **base (asserted)** edges and compute their transitive
    /// closure. Unlike [`Self::from_closed_edges`], this retains the base edges,
    /// so the resulting instance fully supports [`Self::delete_edge`]. Edges are
    /// folded one at a time via the insertion logic (order-independent result).
    pub fn from_base_edges<I: IntoIterator<Item = (u64, u64)>>(edges: I) -> Self {
        let mut c = Self::default();
        for (s, o) in edges {
            c.insert_edge(s, o);
        }
        c
    }

    /// All asserted base edges as `(s, o)` pairs (unordered; caller sorts if
    /// needed). Useful for tests/debugging and for re-seeding.
    pub fn base_edges(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for (&s, os) in &self.base {
            for &o in os {
                out.push((s, o));
            }
        }
        out
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
        // Record the asserted base edge first, independently of the closure
        // delta computation below (which is unchanged). The base set is what
        // makes correct retraction possible.
        self.base.entry(s).or_default().insert(o);

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

    /// Forward-reachable set of `start` over the current **base** adjacency,
    /// inclusive of `start` itself only when `start` lies on a cycle (strict
    /// closure semantics — matches `insert_edge`/`transitive_closure`). The
    /// returned set is the set of `y` such that there is a non-empty base path
    /// `start -> ... -> y`.
    fn base_reach(&self, start: u64) -> FxHashSet<u64> {
        let mut reached: FxHashSet<u64> = FxHashSet::default();
        let mut stack: Vec<u64> = self
            .base
            .get(&start)
            .map(|os| os.iter().copied().collect())
            .unwrap_or_default();
        while let Some(n) = stack.pop() {
            if reached.insert(n) {
                if let Some(os) = self.base.get(&n) {
                    for &m in os {
                        if !reached.contains(&m) {
                            stack.push(m);
                        }
                    }
                }
            }
        }
        reached
    }

    /// Drop the closed pair `(x, y)` from `fwd`/`bwd` and decrement `nnz`.
    /// No-op if the pair is not present.
    fn drop_closed(&mut self, x: u64, y: u64) {
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

    /// Retract one asserted base edge `(s, o)` and return the retraction
    /// [`DeleteOutcome`]: the **withdrawn** closure pairs (positive `(x, y)`
    /// tuples the caller negates) and any **survivor** — the just-deleted base
    /// edge `(s, o)` that is STILL closed-reachable over the remaining base.
    /// Idempotent: if `(s, o)` was not a base edge, returns an empty outcome and
    /// leaves state unchanged. Maintains `closed == transitive_closure(base)`.
    ///
    /// Algorithm (correctness-first affected-region recompute):
    ///  1. Remove `(s, o)` from `base`. If it was absent, return empty.
    ///  2. Candidate region = closed pairs `(x, y)` that *could* lose support:
    ///     `x ∈ closed-bwd[s] ∪ {s}` (everything that reached `s`) and
    ///     `y ∈ closed-fwd[o] ∪ {o}` (everything `o` reached), intersected with
    ///     the pre-delete closure.
    ///  3. For each affected source `x`, recompute its forward reachability over
    ///     the **post-delete** base. A candidate pair `(x, y)` is withdrawn iff
    ///     `y` is no longer base-reachable from `x`.
    ///  4. Drop the withdrawn pairs from the closed adjacency.
    ///  5. The deleted edge `(s, o)` is a **survivor** iff it remained in the
    ///     closed set (i.e. it was NOT withdrawn) — `o` is still reachable from
    ///     `s` over another base path. Report it so the SPEC-06 layer can
    ///     promote it to a materialized derived row (BUG P1).
    pub fn delete_edge(&mut self, s: u64, o: u64) -> DeleteOutcome {
        // 1. Remove from base; bail if it was not an asserted edge.
        let was_base = self
            .base
            .get_mut(&s)
            .map(|set| set.remove(&o))
            .unwrap_or(false);
        if !was_base {
            return DeleteOutcome::default();
        }
        if self.base.get(&s).is_some_and(|set| set.is_empty()) {
            self.base.remove(&s);
        }

        // 2. Affected sources X = closed-bwd[s] ∪ {s}; targets Y = closed-fwd[o] ∪ {o}.
        let mut sources: Vec<u64> = self
            .bwd
            .get(&s)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        sources.push(s);
        let mut targets: FxHashSet<u64> = self
            .fwd
            .get(&o)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        targets.insert(o);

        // 3 + 4. For each affected source, recompute reachability over the
        // post-delete base and withdraw candidate pairs no longer derivable.
        let mut withdrawn = Vec::new();
        for &x in &sources {
            // Candidate targets for this source: closed-fwd[x] ∩ targets.
            let candidates: Vec<u64> = match self.fwd.get(&x) {
                Some(set) => set
                    .iter()
                    .copied()
                    .filter(|y| targets.contains(y))
                    .collect(),
                None => continue,
            };
            if candidates.is_empty() {
                continue;
            }
            let reach = self.base_reach(x);
            for y in candidates {
                if !reach.contains(&y) {
                    withdrawn.push((x, y));
                }
            }
        }

        for &(x, y) in &withdrawn {
            self.drop_closed(x, y);
        }

        // 5. Survivor: the deleted edge `(s, o)` is still in the closed set iff
        // it was NOT withdrawn — i.e. `o` is still reachable from `s` over the
        // remaining base. Only the directly-deleted edge can be a survivor;
        // transitively-implied pairs were already materialized.
        let still_closed = self.fwd.get(&s).is_some_and(|set| set.contains(&o));
        let survived = if still_closed {
            vec![(s, o)]
        } else {
            Vec::new()
        };

        DeleteOutcome {
            withdrawn,
            survived,
        }
    }

    /// Retract many base edges (folded one at a time so each deletion observes
    /// the prior removals) and return the combined retraction outcome
    /// (withdrawn pairs + surviving deleted edges).
    pub fn delete_edges<I: IntoIterator<Item = (u64, u64)>>(&mut self, edges: I) -> DeleteOutcome {
        let mut out = DeleteOutcome::default();
        for (s, o) in edges {
            let one = self.delete_edge(s, o);
            out.withdrawn.extend(one.withdrawn);
            out.survived.extend(one.survived);
        }
        out
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

    #[test]
    fn insert_records_base_edges() {
        let mut c = IncrementalTransitiveClosure::new();
        c.insert_edge(1, 2);
        c.insert_edge(2, 3);
        let mut base = c.base_edges();
        base.sort_unstable();
        // Only the asserted edges, NOT the inferred (1,3).
        assert_eq!(base, vec![(1, 2), (2, 3)]);
    }

    #[test]
    fn from_base_edges_closes_and_retains_base() {
        let c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3)]);
        assert_eq!(edge_set(&c), [(1, 2), (1, 3), (2, 3)].into_iter().collect());
        let mut base = c.base_edges();
        base.sort_unstable();
        assert_eq!(base, vec![(1, 2), (2, 3)]);
    }

    #[test]
    fn delete_non_base_edge_is_noop() {
        // 1->2->3 closed; (1,3) is inferred, not a base edge.
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3)]);
        let pre = edge_set(&c);
        let out = c.delete_edge(1, 3);
        assert!(
            out.withdrawn.is_empty(),
            "deleting an inferred edge must withdraw nothing"
        );
        assert!(
            out.survived.is_empty(),
            "deleting a non-base edge has no survivor"
        );
        assert_eq!(edge_set(&c), pre, "state unchanged on no-op delete");
        // Deleting an edge never asserted at all is also a no-op.
        let out2 = c.delete_edge(7, 8);
        assert!(out2.withdrawn.is_empty() && out2.survived.is_empty());
        assert_eq!(edge_set(&c), pre);
    }

    #[test]
    fn delete_still_supported_via_another_path_withdraws_nothing() {
        // Two base paths from 1 to 3: 1->2->3 and 1->3 (direct).
        // Deleting the direct 1->3 leaves (1,3) supported by 1->2->3.
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (1, 3)]);
        let out = c.delete_edge(1, 3);
        assert!(
            out.withdrawn.is_empty(),
            "(1,3) still derivable via 1->2->3, withdraw nothing; got {:?}",
            out.withdrawn
        );
        // (1,3) was the deleted edge AND is still closed-reachable → survivor.
        assert_eq!(
            out.survived,
            vec![(1, 3)],
            "(1,3) is a survivor: deleted base edge still entailed via 1->2->3"
        );
        assert_eq!(edge_set(&c), [(1, 2), (1, 3), (2, 3)].into_iter().collect());
    }

    #[test]
    fn delete_breaks_chain_withdraws_correct_pairs() {
        // Chain 1->2->3->4. Deleting 2->3 disconnects {1,2} from {3,4}.
        // Withdrawn: (2,3),(2,4),(1,3),(1,4). Surviving: (1,2),(3,4).
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (3, 4)]);
        let mut out = c.delete_edge(2, 3);
        out.withdrawn.sort_unstable();
        assert_eq!(out.withdrawn, vec![(1, 3), (1, 4), (2, 3), (2, 4)]);
        // The deleted edge (2,3) itself was withdrawn (no alternate path), so it
        // is NOT a survivor.
        assert!(
            out.survived.is_empty(),
            "deleted (2,3) was withdrawn, not a survivor"
        );
        assert_eq!(edge_set(&c), [(1, 2), (3, 4)].into_iter().collect());
    }

    #[test]
    fn delete_direct_edge_implied_by_path_reports_survivor() {
        // Base 1->2, 2->3, and direct 1->3. Deleting the DIRECT 1->3 leaves it
        // entailed via 1->2->3: nothing is withdrawn, and (1,3) is reported as a
        // survivor so the SPEC-06 layer can promote it to a derived row (P1).
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (1, 3)]);
        let out = c.delete_edge(1, 3);
        assert!(
            out.withdrawn.is_empty(),
            "nothing withdrawn — alternate path"
        );
        assert_eq!(out.survived, vec![(1, 3)], "(1,3) survives via 1->2->3");
        assert_eq!(edge_set(&c), [(1, 2), (1, 3), (2, 3)].into_iter().collect());
    }

    #[test]
    fn delete_edges_folds_withdrawn_and_survived() {
        // Two independent diamonds folded in one call.
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (1, 3)]);
        // Delete the direct 1->3 (survivor) and the chain edge 2->3 (withdraws
        // the now-broken pairs). Order matters: fold deletes 1->3 first (still
        // entailed via 2->3, survivor), then 2->3 (which now withdraws (1,3) and
        // (2,3) — but (1,3) was already a survivor of the first delete).
        let mut out = c.delete_edges([(1, 3), (2, 3)]);
        out.withdrawn.sort_unstable();
        // After both deletes only (1,2) remains; (1,3),(2,3) are gone.
        assert_eq!(edge_set(&c), [(1, 2)].into_iter().collect());
        // (1,3) survived the first delete (folded into `survived`).
        assert!(
            out.survived.contains(&(1, 3)),
            "got survived={:?}",
            out.survived
        );
    }

    #[test]
    fn delete_then_reinsert_round_trips() {
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (3, 4)]);
        let before = edge_set(&c);
        let before_base = {
            let mut b = c.base_edges();
            b.sort_unstable();
            b
        };
        c.delete_edge(2, 3);
        c.insert_edge(2, 3);
        assert_eq!(
            edge_set(&c),
            before,
            "closure restored after delete+reinsert"
        );
        let after_base = {
            let mut b = c.base_edges();
            b.sort_unstable();
            b
        };
        assert_eq!(
            after_base, before_base,
            "base restored after delete+reinsert"
        );
    }

    #[test]
    fn delete_on_a_cycle_withdraws_self_loops() {
        // 1->2->3->1 is a cycle; strict closure has diagonal {(1,1),(2,2),(3,3)}.
        // Deleting 3->1 breaks the cycle, leaving the chain 1->2->3.
        let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (2, 3), (3, 1)]);
        c.delete_edge(3, 1);
        assert_eq!(
            edge_set(&c),
            [(1, 2), (1, 3), (2, 3)].into_iter().collect(),
            "after breaking the cycle the closure is just the chain"
        );
    }
}
