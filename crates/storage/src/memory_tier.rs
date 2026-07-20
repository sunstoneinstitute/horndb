//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition, DEFAULT_HOT_THRESHOLD};
use crate::term::{GraphId, TermId};
use crate::tier::{Tier, TierStats};
use crate::visibility::UNSET_END;
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// One graph's predicate partitions. Immutable once built; copy-on-write
/// replaces the whole map (sharing untouched partitions by `Arc`) on each write.
#[derive(Default)]
struct GraphStore {
    partitions: HashMap<TermId, Arc<PredicatePartition>>,
}

/// An immutable, versioned view of the entire tier. Readers clone the `Arc`
/// once and are thereafter isolated from concurrent writers, which allocate a
/// fresh `TierSnapshot` and atomically swap the live pointer (copy-on-write).
/// Untouched graphs and partitions are shared between successive snapshots via
/// `Arc`, so a write copies only the affected graph's partition map.
pub struct TierSnapshot {
    version: u64,
    graphs: HashMap<GraphId, Arc<GraphStore>>,
}

impl TierSnapshot {
    fn empty() -> Self {
        Self {
            version: 0,
            graphs: HashMap::new(),
        }
    }

    /// Monotonic version (snapshot id). `0` is the empty store; each successful
    /// `insert_quad_batch` produces the next integer.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Run `f` against the partition for `(graph, predicate)`, if present.
    pub fn with_predicate<F, R>(&self, graph: GraphId, predicate: TermId, f: F) -> Option<R>
    where
        F: FnOnce(&PredicatePartition) -> R,
    {
        self.graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|p| f(p))
    }

    /// Ordered access to a partition in any of the six trie orderings
    /// (SPEC-02 F4). The returned [`crate::partition::OrderedColumns`] owns
    /// `Arc` clones of the columns and so outlives this snapshot borrow.
    pub fn ordered_predicate(
        &self,
        graph: GraphId,
        predicate: TermId,
        ord: crate::ordering::Ordering,
    ) -> Option<crate::partition::OrderedColumns> {
        self.graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|part| part.ordered(ord))
    }

    /// Ordered access to a partition, filtered to rows visible at `self.version`
    /// (SPEC-25 S1) — the version-aware counterpart to [`Self::ordered_predicate`],
    /// which always reads "latest live" regardless of the pinned version.
    pub fn ordered_predicate_at(
        &self,
        graph: GraphId,
        predicate: TermId,
        ord: crate::ordering::Ordering,
    ) -> Option<crate::partition::OrderedColumns> {
        self.graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|part| part.ordered_at(ord, self.version))
    }

    pub fn predicates(&self, graph: GraphId) -> Vec<TermId> {
        self.graphs
            .get(&graph)
            .map(|gs| gs.partitions.keys().copied().collect())
            .unwrap_or_default()
    }

    pub fn graphs(&self) -> Vec<GraphId> {
        self.graphs.keys().copied().collect()
    }

    pub fn triple_count(&self) -> u64 {
        self.graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len_at(self.version) as u64)
            .sum()
    }

    /// The top-`n` predicates in `graph` by triple count, descending. Ties are
    /// broken by predicate id for a deterministic order.
    pub fn top_predicates(&self, graph: GraphId, n: usize) -> Vec<(TermId, u64)> {
        let mut counts: Vec<(TermId, u64)> = self
            .graphs
            .get(&graph)
            .map(|gs| {
                gs.partitions
                    .iter()
                    .map(|(p, part)| (*p, part.len_at(self.version) as u64))
                    .collect()
            })
            .unwrap_or_default();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0 .0.cmp(&b.0 .0)));
        counts.truncate(n);
        counts
    }

    pub fn stats(&self) -> TierStats {
        // Live counts: only graphs/predicates with at least one tuple visible
        // at the pinned version, consistent with `triples` (also version-
        // filtered). After a full delete/CLEAR, retained MVCC history keeps the
        // partitions physically present but they hold no visible rows, so they
        // must not inflate the live graph/predicate counts.
        let mut graphs = 0u64;
        let mut predicates = 0u64;
        for gs in self.graphs.values() {
            let live_preds = gs
                .partitions
                .values()
                .filter(|p| p.len_at(self.version) > 0)
                .count() as u64;
            predicates += live_preds;
            if live_preds > 0 {
                graphs += 1;
            }
        }
        let triples = self.triple_count();
        // Physical footprint spans ALL retained partitions (dead MVCC history
        // costs bytes until compaction): 32 B/row base (16 B for (s, o) + 16 B
        // for the begin/end visibility stamps), plus another 32 B/row when the
        // object-major layout is materialised for a hot predicate; plus
        // ~16 bytes per physically-retained predicate of overhead.
        let physical_predicates: u64 = self
            .graphs
            .values()
            .map(|g| g.partitions.len() as u64)
            .sum();
        let column_bytes: u64 = self
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.estimated_bytes())
            .sum();
        let bytes_estimated = column_bytes + physical_predicates * 16;
        TierStats {
            graphs,
            predicates,
            triples,
            bytes_estimated,
        }
    }
}

pub struct MemoryTier {
    /// The live snapshot pointer. Readers clone the `Arc` under a (cheap,
    /// shared) read lock; the writer swaps in a freshly-built snapshot under the
    /// write lock — held only for the pointer assignment, not the build.
    current: RwLock<Arc<TierSnapshot>>,
    /// Serializes writers (single-writer model): the read-modify-swap in
    /// `insert_quad_batch` must be atomic so concurrent batches can't lose
    /// updates by building from the same base.
    writer: Mutex<()>,
    /// Predicates with at least this many triples eagerly materialise all six
    /// orderings; smaller ones materialise the object-major layout lazily
    /// (SPEC-02 F4).
    hot_threshold: usize,
    /// version -> number of live pins at that version. Empty ⇒ no pins.
    pins: Arc<Mutex<BTreeMap<u64, usize>>>,
}

impl MemoryTier {
    pub fn new() -> Self {
        Self::with_hot_threshold(DEFAULT_HOT_THRESHOLD)
    }

    /// Construct a tier with a custom hot-predicate threshold (SPEC-02 F4).
    pub fn with_hot_threshold(hot_threshold: usize) -> Self {
        Self {
            current: RwLock::new(Arc::new(TierSnapshot::empty())),
            writer: Mutex::new(()),
            hot_threshold,
            pins: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// The hot-predicate triple-count threshold in effect for this tier.
    pub fn hot_threshold(&self) -> usize {
        self.hot_threshold
    }

    /// Pin the current immutable tier state and register the pin so compaction
    /// will not reclaim rows still visible to it. The pin is released when the
    /// returned guard drops.
    pub fn snapshot(&self) -> PinnedSnapshot {
        let snap = self.current.read().clone();
        *self.pins.lock().entry(snap.version).or_insert(0) += 1;
        PinnedSnapshot {
            snap,
            pins: self.pins.clone(),
        }
    }

    /// Lowest pinned version, or the current version if nothing is pinned.
    fn min_pinned(&self) -> u64 {
        // Read `current` before `pins` so every path that touches both locks
        // takes them in the same order (`current` then `pins`), as `snapshot()`
        // does — keeps the ordering deadlock-free for future refactors.
        let cur_version = self.current.read().version;
        let pins = self.pins.lock();
        pins.keys().next().copied().unwrap_or(cur_version)
    }

    /// Reclaim dead rows whose `end <= min_pinned`. Rebuilds only partitions
    /// that actually hold reclaimable rows; never changes a pinned view (those
    /// hold their own older `Arc`s). Does not bump the version — compaction is
    /// not a logical write.
    pub fn compact(&self) {
        let _w = self.writer.lock();
        let horizon = self.min_pinned();
        let cur = self.current.read().clone();
        let mut graphs = cur.graphs.clone();
        let mut changed = false;
        for (g, gs) in cur.graphs.iter() {
            let mut new_partitions = gs.partitions.clone();
            let mut graph_changed = false;
            for (p, part) in gs.partitions.iter() {
                if !part.has_retractions() {
                    continue;
                }
                // Reclaimable iff some row has end <= horizon.
                let reclaimable = (0..part.len()).any(|i| part.ends().value(i) <= horizon);
                if !reclaimable {
                    continue;
                }
                let mut builder = PartitionBuilder::default();
                for i in 0..part.len() {
                    let end = part.ends().value(i);
                    if end <= horizon {
                        continue; // reclaim
                    }
                    builder.append_stamped(
                        TermId(part.subjects().value(i)),
                        TermId(part.objects().value(i)),
                        part.begins().value(i),
                        end,
                    );
                }
                new_partitions.insert(
                    *p,
                    Arc::new(builder.build_with_hot_threshold(self.hot_threshold)),
                );
                graph_changed = true;
            }
            if graph_changed {
                graphs.insert(
                    *g,
                    Arc::new(GraphStore {
                        partitions: new_partitions,
                    }),
                );
                changed = true;
            }
        }
        if changed {
            // Same version: compaction is not a logical write.
            let next = Arc::new(TierSnapshot {
                version: cur.version,
                graphs,
            });
            *self.current.write() = next;
        }
    }
}

impl Default for MemoryTier {
    fn default() -> Self {
        Self::new()
    }
}

/// A pinned tier snapshot that keeps its version un-compactable until dropped.
pub struct PinnedSnapshot {
    snap: Arc<TierSnapshot>,
    pins: Arc<Mutex<BTreeMap<u64, usize>>>,
}

impl std::ops::Deref for PinnedSnapshot {
    type Target = TierSnapshot;
    fn deref(&self) -> &TierSnapshot {
        &self.snap
    }
}

impl PinnedSnapshot {
    /// The pinned immutable tier state, as a cloneable `Arc`.
    pub fn arc(&self) -> Arc<TierSnapshot> {
        self.snap.clone()
    }
}

impl Drop for PinnedSnapshot {
    fn drop(&mut self) {
        let v = self.snap.version;
        let mut pins = self.pins.lock();
        if let Some(count) = pins.get_mut(&v) {
            *count -= 1;
            if *count == 0 {
                pins.remove(&v);
            }
        }
    }
}

impl Tier for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        if quads.is_empty() {
            return Ok(());
        }
        // Group incoming pairs by graph, then predicate.
        let mut by_graph: HashMap<GraphId, HashMap<TermId, Vec<(TermId, TermId)>>> = HashMap::new();
        for &(g, s, p, o) in quads {
            by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .push((s, o));
        }

        // Serialize writers so the read-modify-swap is atomic.
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = cur.version + 1;

        // Copy-on-write: clone the top-level graph map (Arc clones of untouched
        // graphs), then rebuild only the affected graphs' partition maps.
        let mut graphs = cur.graphs.clone();
        for (g, pred_rows) in by_graph {
            let mut new_partitions = graphs
                .get(&g)
                .map(|gs| gs.partitions.clone())
                .unwrap_or_default();
            for (p, rows) in pred_rows {
                let mut builder = PartitionBuilder::default();
                // Carry existing rows forward WITH their visibility stamps
                // (history preserved) — reading indexed columns, not `scan()`,
                // which would silently drop retraction stamps.
                if let Some(existing) = new_partitions.get(&p) {
                    let n = existing.len();
                    for i in 0..n {
                        builder.append_stamped(
                            TermId(existing.subjects().value(i)),
                            TermId(existing.objects().value(i)),
                            existing.begins().value(i),
                            existing.ends().value(i),
                        );
                    }
                }
                // New rows: live from this version.
                for (s, o) in rows {
                    builder.append_stamped(s, o, new_version, UNSET_END);
                }
                new_partitions.insert(
                    p,
                    Arc::new(builder.build_with_hot_threshold(self.hot_threshold)),
                );
            }
            graphs.insert(
                g,
                Arc::new(GraphStore {
                    partitions: new_partitions,
                }),
            );
        }

        let next = Arc::new(TierSnapshot {
            version: new_version,
            graphs,
        });
        *self.current.write() = next;
        Ok(())
    }

    fn retract_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<usize> {
        if quads.is_empty() {
            return Ok(0);
        }
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = cur.version + 1;

        // Group targets by graph, then predicate, as a set of (s, o) to end.
        let mut by_graph: HashMap<GraphId, HashMap<TermId, HashSet<(u64, u64)>>> = HashMap::new();
        for &(g, s, p, o) in quads {
            by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .insert((s.0, o.0));
        }

        let mut retracted = 0usize;
        let mut graphs = cur.graphs.clone();
        for (g, pred_targets) in by_graph {
            let Some(gs) = graphs.get(&g) else {
                continue;
            };
            let mut new_partitions = gs.partitions.clone();
            for (p, targets) in pred_targets {
                let Some(existing) = new_partitions.get(&p) else {
                    continue;
                };
                let mut builder = PartitionBuilder::default();
                let n = existing.len();
                for i in 0..n {
                    let s = existing.subjects().value(i);
                    let o = existing.objects().value(i);
                    let begin = existing.begins().value(i);
                    let mut end = existing.ends().value(i);
                    // End the single live row matching a target.
                    if end == UNSET_END && targets.contains(&(s, o)) {
                        end = new_version;
                        retracted += 1;
                    }
                    builder.append_stamped(TermId(s), TermId(o), begin, end);
                }
                new_partitions.insert(
                    p,
                    Arc::new(builder.build_with_hot_threshold(self.hot_threshold)),
                );
            }
            graphs.insert(
                g,
                Arc::new(GraphStore {
                    partitions: new_partitions,
                }),
            );
        }

        // Only bump the clock / swap if something changed, so a fully-absent
        // retraction batch is a true no-op (no dead version created).
        if retracted > 0 {
            let next = Arc::new(TierSnapshot {
                version: new_version,
                graphs,
            });
            *self.current.write() = next;
        }
        Ok(retracted)
    }

    fn predicate(&self, _graph: GraphId, _predicate: TermId) -> Option<&PredicatePartition> {
        // Returning `&PredicatePartition` across the snapshot pointer would
        // require a guard-bound borrow. Stage-1 callers use the guarded
        // accessors on `TierSnapshot` (`with_predicate` / `ordered_predicate`)
        // obtained via `MemoryTier::snapshot`; this stub stays for forward
        // compatibility with the `Tier` trait.
        None
    }

    fn predicates(&self, graph: GraphId) -> Vec<TermId> {
        self.snapshot().predicates(graph)
    }

    fn graphs(&self) -> Vec<GraphId> {
        self.snapshot().graphs()
    }

    fn triple_count(&self) -> u64 {
        self.snapshot().triple_count()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn stats(&self) -> TierStats {
        self.snapshot().stats()
    }
}

impl MemoryTier {
    /// Guarded accessor for a partition in the **current** snapshot. The closure
    /// runs against a pinned snapshot, so it is consistent for its duration.
    pub fn with_predicate<F, R>(&self, graph: GraphId, predicate: TermId, f: F) -> Option<R>
    where
        F: FnOnce(&PredicatePartition) -> R,
    {
        self.snapshot().with_predicate(graph, predicate, f)
    }

    /// Ordered access to a predicate partition in the current snapshot
    /// (SPEC-02 F4). See [`TierSnapshot::ordered_predicate`].
    pub fn ordered_predicate(
        &self,
        graph: GraphId,
        predicate: TermId,
        ord: crate::ordering::Ordering,
    ) -> Option<crate::partition::OrderedColumns> {
        self.snapshot().ordered_predicate(graph, predicate, ord)
    }

    /// Ordered access to a predicate partition in the current snapshot,
    /// filtered to rows visible at that snapshot's version (SPEC-25 S1). See
    /// [`TierSnapshot::ordered_predicate_at`].
    pub fn ordered_predicate_at(
        &self,
        graph: GraphId,
        predicate: TermId,
        ord: crate::ordering::Ordering,
    ) -> Option<crate::partition::OrderedColumns> {
        self.snapshot().ordered_predicate_at(graph, predicate, ord)
    }

    /// The top-`n` predicates in `graph` by triple count in the current
    /// snapshot, descending (deterministic tie-break by predicate id).
    pub fn top_predicates(&self, graph: GraphId, n: usize) -> Vec<(TermId, u64)> {
        self.snapshot().top_predicates(graph, n)
    }
}

#[cfg(test)]
mod tests {
    use crate::memory_tier::MemoryTier;
    use crate::term::{GraphId, TermId, TermKind, DEFAULT_GRAPH};
    use crate::tier::Tier;

    fn id(payload: u64) -> TermId {
        TermId::new(TermKind::Uri, payload)
    }

    #[test]
    fn snapshot_is_pinned_against_later_writes() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        // Pin a snapshot of the one-triple state.
        let snap = tier.snapshot();
        assert_eq!(snap.version(), 1);
        assert_eq!(snap.triple_count(), 1);

        // A later write must not change the pinned snapshot.
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(3), id(100), id(4))])
            .unwrap();
        assert_eq!(snap.triple_count(), 1, "pinned snapshot saw a later write");
        assert_eq!(snap.version(), 1);

        // The live tier reflects the write and a newer version.
        let live = tier.snapshot();
        assert_eq!(live.triple_count(), 2);
        assert_eq!(live.version(), 2);
    }

    #[test]
    fn empty_tier_starts_at_version_zero() {
        let tier = MemoryTier::new();
        assert_eq!(tier.snapshot().version(), 0);
        assert_eq!(tier.snapshot().triple_count(), 0);
    }

    #[test]
    fn insert_and_count() {
        let tier = MemoryTier::new();
        let quads = vec![
            (DEFAULT_GRAPH, id(1), id(100), id(2)),
            (DEFAULT_GRAPH, id(1), id(100), id(3)),
            (DEFAULT_GRAPH, id(1), id(101), id(2)),
        ];
        tier.insert_quad_batch(&quads).unwrap();
        assert_eq!(tier.triple_count(), 3);
        let mut preds = tier.predicates(DEFAULT_GRAPH);
        preds.sort_by_key(|t| t.0);
        assert_eq!(preds, vec![id(100), id(101)]);
    }

    #[test]
    fn batched_inserts_merge_into_one_partition() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(3), id(100), id(4))])
            .unwrap();
        let pairs = tier
            .with_predicate(DEFAULT_GRAPH, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(pairs.len(), 2);
        // SPO sort: subject 1 < subject 3.
        assert_eq!(pairs[0].0, id(1));
        assert_eq!(pairs[1].0, id(3));
    }

    #[test]
    fn named_graphs_are_isolated() {
        let tier = MemoryTier::new();
        let g1 = GraphId(TermId::new(TermKind::Uri, 10).0);
        let g2 = GraphId(TermId::new(TermKind::Uri, 11).0);
        tier.insert_quad_batch(&[(g1, id(1), id(100), id(2)), (g2, id(1), id(100), id(3))])
            .unwrap();
        let g1_pairs = tier
            .with_predicate(g1, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        let g2_pairs = tier
            .with_predicate(g2, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(g1_pairs, vec![(id(1), id(2))]);
        assert_eq!(g2_pairs, vec![(id(1), id(3))]);
    }

    #[test]
    fn retract_hides_from_later_snapshot_only() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        let before = tier.snapshot(); // version 1, sees the tuple
        let n = tier
            .retract_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        assert_eq!(n, 1, "one tuple retracted");
        let after = tier.snapshot(); // version 2, tuple gone

        assert_eq!(
            before.triple_count(),
            1,
            "snapshot pinned before delete still sees it"
        );
        assert_eq!(after.triple_count(), 0, "snapshot after delete does not");
    }

    #[test]
    fn retract_absent_is_counted_noop() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        // Retract a tuple that was never inserted.
        let n = tier
            .retract_quad_batch(&[(DEFAULT_GRAPH, id(9), id(100), id(9))])
            .unwrap();
        assert_eq!(n, 0, "absent retraction retracts nothing");
        assert_eq!(tier.snapshot().triple_count(), 1);
        assert_eq!(
            tier.snapshot().version(),
            1,
            "absent retraction must not mint a new version"
        );
    }

    #[test]
    fn reinsert_after_retract_is_live_again() {
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q]).unwrap();
        tier.retract_quad_batch(&[q]).unwrap();
        tier.insert_quad_batch(&[q]).unwrap();
        assert_eq!(
            tier.snapshot().triple_count(),
            1,
            "tuple live after re-insert"
        );
    }

    #[test]
    fn compaction_reclaims_only_below_min_pin() {
        let tier = MemoryTier::new();
        let q1 = (DEFAULT_GRAPH, id(1), id(100), id(2));
        let q2 = (DEFAULT_GRAPH, id(3), id(100), id(4));
        tier.insert_quad_batch(&[q1, q2]).unwrap(); // v1
        tier.retract_quad_batch(&[q1]).unwrap(); // v2: q1.end = 2

        // No pins below v2 → q1's dead row is reclaimable.
        tier.compact();
        let live = tier.snapshot();
        assert_eq!(live.triple_count(), 1);
        // The physical dead row is gone: the partition holds exactly the live row.
        let phys = tier
            .with_predicate(DEFAULT_GRAPH, id(100), |p| p.len())
            .unwrap();
        assert_eq!(phys, 1, "dead row physically reclaimed");
    }

    #[test]
    fn compaction_respects_a_held_pin() {
        let tier = MemoryTier::new();
        let q1 = (DEFAULT_GRAPH, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q1]).unwrap(); // v1
        let pin = tier.snapshot(); // pins v1 (sees q1)
        tier.retract_quad_batch(&[q1]).unwrap(); // v2

        tier.compact(); // min pin = 1 < end(2) → must NOT reclaim
        assert_eq!(pin.triple_count(), 1, "held pin still sees the tuple");
        drop(pin);
    }

    #[test]
    fn stats_live_counts_drop_to_zero_after_full_retraction() {
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q]).unwrap();
        let s = tier.stats();
        assert_eq!((s.graphs, s.predicates, s.triples), (1, 1, 1));

        // Retract the only tuple: the partition is retained as MVCC history but
        // holds no visible row, so live graph/predicate/triple counts are 0.
        tier.retract_quad_batch(&[q]).unwrap();
        let s = tier.stats();
        assert_eq!(
            (s.graphs, s.predicates, s.triples),
            (0, 0, 0),
            "fully-deleted graph/predicate must not inflate live stats"
        );
        // Physical footprint still accounts for the retained (dead) partition.
        assert!(s.bytes_estimated > 0, "retained history still costs bytes");
    }
}
