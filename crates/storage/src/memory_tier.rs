//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition, DEFAULT_HOT_THRESHOLD};
use crate::term::{GraphId, TermId};
use crate::tier::{ApplyReport, Tier, TierStats};
use crate::visibility::UNSET_END;
use horndb_metrics::labels::LoadPhase;
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    /// Number of visible triples in `g` at this snapshot's version. One graph
    /// lookup, no allocation. An absent or never-interned graph yields 0.
    pub fn graph_len(&self, g: GraphId) -> usize {
        self.graphs
            .get(&g)
            .map(|gs| gs.partitions.values().map(|p| p.live_len()).sum())
            .unwrap_or(0)
    }

    /// The graphs with >=1 visible quad (D11 — see [`crate::tier::Tier::graphs`]
    /// for the full contract). Shares [`Self::partition_is_live`] with
    /// [`Self::stats`] so the two live counts never disagree. Order is
    /// arbitrary (`HashMap` iteration order); callers needing determinism use
    /// [`crate::store::StoreSnapshot::graphs`].
    pub fn graphs(&self) -> Vec<GraphId> {
        self.graphs
            .iter()
            .filter(|(_, gs)| gs.partitions.values().any(|p| Self::partition_is_live(p)))
            .map(|(g, _)| *g)
            .collect()
    }

    /// True if `p` has at least one live row. Version-independent by
    /// construction: see `PredicatePartition::live_len`.
    fn partition_is_live(p: &PredicatePartition) -> bool {
        p.live_len() > 0
    }

    pub fn triple_count(&self) -> u64 {
        self.graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.live_len() as u64)
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
                .filter(|p| Self::partition_is_live(p))
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

/// Merge one bulk-load phase into the shared counters. Called once per phase
/// per batch, never from inside a loop (SPEC-17 §5.4).
fn record_phase(phase: LoadPhase, elapsed: Duration, rows: u64) {
    horndb_metrics::metrics()
        .storage
        .record_load_phase(phase, elapsed, rows);
}

impl Tier for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        if quads.is_empty() {
            return Ok(());
        }
        // Group incoming pairs by graph, then predicate. One clock read on each
        // side of the loop; nothing inside it touches a metric (SPEC-17 §5.4).
        let t_group = Instant::now();
        let mut by_graph: HashMap<GraphId, HashMap<TermId, Vec<(TermId, TermId)>>> = HashMap::new();
        for &(g, s, p, o) in quads {
            by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .push((s, o));
        }
        record_phase(LoadPhase::Group, t_group.elapsed(), quads.len() as u64);

        // Nanoseconds and rows accumulated in locals across every partition
        // touched by this batch, merged into the counters once on the way out.
        let mut copy_ns = 0u64;
        let mut build_ns = 0u64;
        let mut build_rows = 0u64;

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
                // New rows: live from this version.
                let added = rows.len() as u64;
                let t_copy = Instant::now();
                let new_rows: Vec<_> = rows
                    .into_iter()
                    .map(|(s, o)| (s.0, o.0, new_version, UNSET_END))
                    .collect();
                copy_ns += t_copy.elapsed().as_nanos() as u64;

                // The existing rows are shared by `Arc` as an untouched run;
                // only the new rows are sorted here. The merged view is built
                // once, on the first read (HDB-84) — so `build` is the new
                // rows, not the whole partition.
                let t_build = Instant::now();
                let part = match new_partitions.get(&p) {
                    Some(existing) => existing.with_appended_rows(new_rows),
                    None => PartitionBuilder::from_rows(new_rows)
                        .build_with_hot_threshold(self.hot_threshold),
                };
                new_partitions.insert(p, Arc::new(part));
                build_ns += t_build.elapsed().as_nanos() as u64;
                build_rows += added;
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

        // Single merge point for the per-partition accumulators. Nothing is
        // carried forward any more, so `copy_forward` covers only staging the
        // new rows and its row count is the new rows.
        record_phase(
            LoadPhase::CopyForward,
            Duration::from_nanos(copy_ns),
            build_rows,
        );
        record_phase(LoadPhase::Build, Duration::from_nanos(build_ns), build_rows);
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

    fn apply_quad_batch(
        &self,
        dels: &[(GraphId, TermId, TermId, TermId)],
        adds: &[(GraphId, TermId, TermId, TermId)],
    ) -> Result<ApplyReport> {
        if dels.is_empty() && adds.is_empty() {
            return Ok(ApplyReport::default());
        }
        // Group each side by graph, then predicate, as a set of (s, o)
        // payload pairs — a `HashSet` absorbs in-batch duplicate targets so
        // they are not double-counted.
        let t_group = Instant::now();
        let mut del_by_graph: HashMap<GraphId, HashMap<TermId, HashSet<(u64, u64)>>> =
            HashMap::new();
        for &(g, s, p, o) in dels {
            del_by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .insert((s.0, o.0));
        }
        let mut add_by_graph: HashMap<GraphId, HashMap<TermId, HashSet<(u64, u64)>>> =
            HashMap::new();
        for &(g, s, p, o) in adds {
            add_by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .insert((s.0, o.0));
        }
        record_phase(
            LoadPhase::Group,
            t_group.elapsed(),
            (dels.len() + adds.len()) as u64,
        );

        // Per-partition accumulators, merged into the counters once at the end.
        let mut copy_ns = 0u64;
        let mut copy_rows = 0u64;
        let mut merge_ns = 0u64;
        let mut merge_rows = 0u64;
        let mut build_ns = 0u64;
        let mut build_rows = 0u64;

        // Serialize writers so the read-modify-swap is atomic.
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = cur.version + 1;

        let mut retracted = 0usize;
        let mut inserted = 0usize;
        let mut graphs = cur.graphs.clone();

        let touched_graphs: HashSet<GraphId> = del_by_graph
            .keys()
            .chain(add_by_graph.keys())
            .copied()
            .collect();

        for g in touched_graphs {
            let del_preds = del_by_graph.get(&g);
            let add_preds = add_by_graph.get(&g);
            let graph_existed = cur.graphs.contains_key(&g);
            let mut new_partitions = graphs
                .get(&g)
                .map(|gs| gs.partitions.clone())
                .unwrap_or_default();

            let touched_preds: HashSet<TermId> = del_preds
                .into_iter()
                .flat_map(|m| m.keys())
                .chain(add_preds.into_iter().flat_map(|m| m.keys()))
                .copied()
                .collect();

            for p in touched_preds {
                let del_targets = del_preds.and_then(|m| m.get(&p));
                let add_targets = add_preds.and_then(|m| m.get(&p));
                let has_existing = new_partitions.contains_key(&p);
                if !has_existing && add_targets.is_none() {
                    // Nothing to retract from (partition doesn't exist) and
                    // nothing to add: a true no-op for this predicate.
                    continue;
                }

                // One pass over the existing rows: end-stamp del matches
                // (dels apply before adds), carry every row forward, and
                // track which (s, o) pairs remain visible so the add pass
                // below knows what is genuinely new.
                let mut builder = PartitionBuilder::default();
                let mut still_visible: HashSet<(u64, u64)> = HashSet::new();
                let mut carried = 0u64;
                let t_copy = Instant::now();
                if let Some(existing) = new_partitions.get(&p) {
                    let n = existing.len();
                    carried = n as u64;
                    for i in 0..n {
                        let s = existing.subjects().value(i);
                        let o = existing.objects().value(i);
                        let begin = existing.begins().value(i);
                        let mut end = existing.ends().value(i);
                        if end == UNSET_END {
                            if del_targets.is_some_and(|t| t.contains(&(s, o))) {
                                end = new_version;
                                retracted += 1;
                            } else {
                                still_visible.insert((s, o));
                            }
                        }
                        builder.append_stamped(TermId(s), TermId(o), begin, end);
                    }
                }
                copy_ns += t_copy.elapsed().as_nanos() as u64;
                copy_rows += carried;

                let mut added = 0u64;
                let t_merge = Instant::now();
                if let Some(targets) = add_targets {
                    added = targets.len() as u64;
                    for &(s, o) in targets {
                        // Visible "after the dels" — a quad ended above by
                        // this same batch is not in `still_visible`, so a
                        // del+add of the same quad within one batch counts
                        // as both a retract and a fresh insert.
                        if !still_visible.contains(&(s, o)) {
                            builder.append_stamped(TermId(s), TermId(o), new_version, UNSET_END);
                            inserted += 1;
                        }
                    }
                }
                merge_ns += t_merge.elapsed().as_nanos() as u64;
                merge_rows += added;

                let t_build = Instant::now();
                new_partitions.insert(
                    p,
                    Arc::new(builder.build_with_hot_threshold(self.hot_threshold)),
                );
                build_ns += t_build.elapsed().as_nanos() as u64;
                build_rows += carried + added;
            }

            if new_partitions.is_empty() && !graph_existed {
                // Never had this graph, and this batch didn't add to it.
                continue;
            }
            graphs.insert(
                g,
                Arc::new(GraphStore {
                    partitions: new_partitions,
                }),
            );
        }

        // Only bump the clock / swap the live pointer if the batch's net
        // effect is non-empty — a replayed or already-applied batch must not
        // invalidate every reader snapshot for nothing.
        if retracted > 0 || inserted > 0 {
            let next = Arc::new(TierSnapshot {
                version: new_version,
                graphs,
            });
            *self.current.write() = next;
        }

        record_phase(
            LoadPhase::CopyForward,
            Duration::from_nanos(copy_ns),
            copy_rows,
        );
        record_phase(LoadPhase::Merge, Duration::from_nanos(merge_ns), merge_rows);
        record_phase(LoadPhase::Build, Duration::from_nanos(build_ns), build_rows);

        Ok(ApplyReport {
            retracted,
            inserted,
        })
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

    /// HDB-84: the tier appends each batch as its own run and merges them on
    /// first read, so the same rows split N ways must produce a partition no
    /// reader can tell from the one-batch build — every column, both axes,
    /// both side-sets, and the live count.
    #[test]
    fn batching_does_not_change_the_partition() {
        use crate::ordering::Ordering;

        let pred = id(100);
        // Deliberate repeats: (s, o) pairs recur across chunk boundaries, so
        // cross-run dedup has to behave like the single-build dedup.
        let quads: Vec<_> = (0..500u64)
            .map(|i| (DEFAULT_GRAPH, id(i % 97), pred, id(i % 31)))
            .collect();

        let one = MemoryTier::new();
        one.insert_quad_batch(&quads).unwrap();

        for chunk in [1usize, 3, 7, 64, 499] {
            let many = MemoryTier::new();
            for part in quads.chunks(chunk) {
                many.insert_quad_batch(part).unwrap();
            }
            let (a, b) = (one.snapshot(), many.snapshot());
            let read = |snap: &crate::memory_tier::TierSnapshot| {
                snap.with_predicate(DEFAULT_GRAPH, pred, |p| {
                    (
                        p.len(),
                        p.live_len(),
                        p.has_retractions(),
                        p.subject_set().clone(),
                        p.object_set().clone(),
                        p.ordered(Ordering::Spo)
                            .subject_object()
                            .collect::<Vec<_>>(),
                        p.ordered(Ordering::Pos)
                            .subject_object()
                            .collect::<Vec<_>>(),
                    )
                })
                .unwrap()
            };
            assert_eq!(read(&a), read(&b), "chunk size {chunk}");
            assert_eq!(a.triple_count(), b.triple_count(), "chunk size {chunk}");
        }
    }

    /// Retractions and re-inserts land on a partition that is still a list of
    /// unmerged runs. The visible rows must match the same operations replayed
    /// one batch at a time on a fully merged partition.
    #[test]
    fn retraction_after_batched_inserts_sees_every_run() {
        let pred = id(100);
        let quads: Vec<_> = (0..64u64)
            .map(|i| (DEFAULT_GRAPH, id(i), pred, id(i + 1000)))
            .collect();

        let tier = MemoryTier::new();
        for part in quads.chunks(5) {
            tier.insert_quad_batch(part).unwrap();
        }
        // Retract every fourth row, spread across the runs.
        let targets: Vec<_> = quads.iter().step_by(4).copied().collect();
        assert_eq!(tier.retract_quad_batch(&targets).unwrap(), targets.len());
        assert_eq!(tier.triple_count(), (quads.len() - targets.len()) as u64);

        // Re-inserting a retracted quad makes it live again, still in batches.
        tier.insert_quad_batch(&targets[..4]).unwrap();
        assert_eq!(
            tier.triple_count(),
            (quads.len() - targets.len() + 4) as u64
        );
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

/// Tests for `Tier::apply_quad_batch` (SPEC-28 S6, PLAN-28-04 task 1) — the
/// combined dels-then-adds atomic path. A separate module (not nested in
/// `tests` above) so `retract_absent_is_counted_noop` can share its name with
/// the pre-existing `retract_quad_batch`-only test: same behaviour, exercised
/// through the new combined path.
#[cfg(test)]
mod apply_quad_batch_tests {
    use crate::memory_tier::MemoryTier;
    use crate::term::{GraphId, TermId, TermKind, DEFAULT_GRAPH};
    use crate::tier::Tier;

    fn id(payload: u64) -> TermId {
        TermId::new(TermKind::Uri, payload)
    }

    #[test]
    fn apply_is_one_commit_version() {
        let tier = MemoryTier::new();
        let old_quad = (DEFAULT_GRAPH, id(1), id(100), id(2));
        let new_quad = (DEFAULT_GRAPH, id(3), id(100), id(4));
        tier.insert_quad_batch(&[old_quad]).unwrap(); // v1
        let before = tier.snapshot(); // pinned at v1: sees old_quad only

        let report = tier.apply_quad_batch(&[old_quad], &[new_quad]).unwrap();
        assert_eq!(report.retracted, 1);
        assert_eq!(report.inserted, 1);

        let after = tier.snapshot();
        assert_eq!(before.version(), 1);
        assert_eq!(after.version(), 2, "one batch = exactly one version bump");

        // The pinned-before reader sees neither the retraction nor the
        // insertion: old_quad still present, new_quad still absent.
        let before_pairs = before
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(before.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(before_pairs, vec![(id(1), id(2))]);

        let after_pairs = after
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(after.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(after_pairs, vec![(id(3), id(4))]);
    }

    #[test]
    fn dels_before_adds_within_batch() {
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));

        // Batch N: plain insert.
        tier.insert_quad_batch(&[q]).unwrap(); // v1

        // Batch N+1: delete AND re-add the same quad in one call.
        let report = tier.apply_quad_batch(&[q], &[q]).unwrap();
        assert_eq!(report.retracted, 1, "the live row from batch N is ended");
        assert_eq!(report.inserted, 1, "a fresh row is inserted after the del");

        let snap = tier.snapshot();
        assert_eq!(
            snap.version(),
            2,
            "one commit version for the combined batch"
        );
        // `scan_at` (not raw `scan`): the dead row from batch N and the live
        // row from batch N+1 are both physically retained as MVCC history —
        // only the visibility-filtered read must show the quad exactly once.
        let live = snap
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(snap.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(live, vec![(id(1), id(2))], "the quad ends present");
    }

    #[test]
    fn insert_present_is_counted_noop() {
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q]).unwrap(); // v1

        let report = tier.apply_quad_batch(&[], &[q]).unwrap();
        assert_eq!(report.inserted, 0, "already-live quad is not recounted");
        assert_eq!(report.retracted, 0);
        assert_eq!(
            tier.snapshot().version(),
            1,
            "a no-op insert must not bump the version"
        );
    }

    #[test]
    fn retract_absent_is_counted_noop() {
        // Existing `retract_quad_batch` behaviour, now exercised through the
        // combined `apply_quad_batch` path.
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap(); // v1

        let report = tier
            .apply_quad_batch(&[(DEFAULT_GRAPH, id(9), id(100), id(9))], &[])
            .unwrap();
        assert_eq!(report.retracted, 0, "absent retraction retracts nothing");
        assert_eq!(report.inserted, 0);
        assert_eq!(tier.snapshot().triple_count(), 1);
        assert_eq!(
            tier.snapshot().version(),
            1,
            "absent retraction must not mint a new version"
        );
    }

    #[test]
    fn noop_batch_does_not_bump_version() {
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q]).unwrap(); // v1

        // Del target absent + add target already present: net-empty batch.
        let report = tier
            .apply_quad_batch(&[(DEFAULT_GRAPH, id(9), id(100), id(9))], &[q])
            .unwrap();
        assert_eq!((report.retracted, report.inserted), (0, 0));
        assert_eq!(
            tier.snapshot().version(),
            1,
            "a net-empty batch must not bump the version"
        );

        // A literally empty batch is also a no-op.
        let report2 = tier.apply_quad_batch(&[], &[]).unwrap();
        assert_eq!((report2.retracted, report2.inserted), (0, 0));
        assert_eq!(tier.snapshot().version(), 1);
    }

    #[test]
    fn quad_identity_is_per_graph() {
        let tier = MemoryTier::new();
        let g1 = GraphId(TermId::new(TermKind::Uri, 10).0);
        let g2 = GraphId(TermId::new(TermKind::Uri, 11).0);
        // Same (s, p, o) triple, asserted in two different graphs.
        let q1 = (g1, id(1), id(100), id(2));
        let q2 = (g2, id(1), id(100), id(2));
        tier.insert_quad_batch(&[q1, q2]).unwrap(); // v1

        let report = tier.apply_quad_batch(&[q1], &[]).unwrap();
        assert_eq!(report.retracted, 1);

        let snap = tier.snapshot();
        let g1_pairs = snap
            .with_predicate(g1, id(100), |p| p.scan_at(snap.version()).count())
            .unwrap_or(0);
        assert_eq!(g1_pairs, 0, "g1's quad was retracted");
        let g2_pairs = snap
            .with_predicate(g2, id(100), |p| {
                p.scan_at(snap.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(
            g2_pairs,
            vec![(id(1), id(2))],
            "g2's identical quad survives"
        );
    }
}
