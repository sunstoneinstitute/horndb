//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::cold::{cold_path, ColdPartition};
use crate::error::Result;
use crate::partition::{Partition, PartitionBuilder, PredicatePartition};
use crate::term::{GraphId, TermId};
use crate::tier::{ApplyReport, Tier, TierStats, TierWrite};
use crate::tiering::{PlacementHints, RebalanceReport, TieringConfig};
use crate::visibility::UNSET_END;
use dashmap::DashMap;
use horndb_metrics::labels::LoadPhase;
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Per-partition read counters, the input to the placement policy
/// ([`MemoryTier::rebalance`], SPEC-25 S5). Shared by `Arc` across every
/// successive [`TierSnapshot`], so the counts survive writes, compaction and
/// tier moves — they describe access to a `(graph, predicate)` partition, not
/// to one snapshot's copy of it.
#[derive(Default)]
pub struct AccessStats {
    hits: DashMap<(GraphId, TermId), AtomicU64>,
}

impl AccessStats {
    /// Count one read of `(graph, predicate)`.
    ///
    /// ponytail: one hash lookup per partition access. Move the counter into
    /// `Partition` (where the lookup already happened) if a profile shows it.
    fn hit(&self, graph: GraphId, predicate: TermId) {
        let key = (graph, predicate);
        // Fast path: a shard read lock. Only the first access of a partition
        // takes the shard's write lock.
        if let Some(c) = self.hits.get(&key) {
            c.fetch_add(1, AtomicOrdering::Relaxed);
            return;
        }
        self.hits
            .entry(key)
            .or_default()
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// Every partition's count since the last `take`, zeroing each counter.
    /// Order is arbitrary (`DashMap` iteration order); the policy sorts.
    pub fn take(&self) -> Vec<((GraphId, TermId), u64)> {
        self.hits
            .iter()
            .map(|e| (*e.key(), e.value().swap(0, AtomicOrdering::Relaxed)))
            .collect()
    }
}

/// The warm form of `part`, promoting a cold partition into `scratch` first.
///
/// Writes never land on a cold partition (SPEC-25 S5): every write path needs
/// the visibility stamp columns, which only a warm partition has, so a cold one
/// is decoded back into a `PredicatePartition` and the write proceeds against
/// that. `scratch` owns the promoted partition for the caller's borrow.
fn warm_for_write<'a>(
    part: &'a Partition,
    scratch: &'a mut Option<PredicatePartition>,
    hot_threshold: usize,
) -> &'a PredicatePartition {
    match part {
        Partition::Warm(w) => w,
        Partition::Cold(c) => scratch.insert(c.promote(hot_threshold)),
    }
}

/// One graph's predicate partitions. Immutable once built; copy-on-write
/// replaces the whole map (sharing untouched partitions by `Arc`) on each write.
#[derive(Default)]
struct GraphStore {
    partitions: HashMap<TermId, Arc<Partition>>,
}

/// An immutable, versioned view of the entire tier. Readers clone the `Arc`
/// once and are thereafter isolated from concurrent writers, which allocate a
/// fresh `TierSnapshot` and atomically swap the live pointer (copy-on-write).
/// Untouched graphs and partitions are shared between successive snapshots via
/// `Arc`, so a write copies only the affected graph's partition map.
pub struct TierSnapshot {
    version: u64,
    graphs: HashMap<GraphId, Arc<GraphStore>>,
    /// Read counts per partition. Cloned (`Arc`) into every successor
    /// snapshot, so one instance per tier — see [`AccessStats`].
    access: Arc<AccessStats>,
}

impl TierSnapshot {
    fn empty() -> Self {
        Self {
            version: 0,
            graphs: HashMap::new(),
            access: Arc::new(AccessStats::default()),
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
        F: FnOnce(&Partition) -> R,
    {
        self.graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|p| {
                self.access.hit(graph, predicate);
                f(p)
            })
    }

    /// The uncounted sibling of [`Self::with_predicate`], for whole-store and
    /// whole-graph maintenance sweeps that visit every predicate
    /// (`Store::checkpoint`, `scan_graph`, `iter_graph_term_ids`, and
    /// `export_snapshot` via the latter): a sweep touching every partition in
    /// a graph says nothing about any one partition's locality, so it must
    /// not count as a read that keeps a partition warm (SPEC-25 S5).
    pub fn with_predicate_uncounted<F, R>(
        &self,
        graph: GraphId,
        predicate: TermId,
        f: F,
    ) -> Option<R>
    where
        F: FnOnce(&Partition) -> R,
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
            .map(|part| {
                self.access.hit(graph, predicate);
                part.ordered(ord)
            })
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
            .map(|part| {
                self.access.hit(graph, predicate);
                part.ordered_at(ord, self.version)
            })
    }

    /// Whether `(graph, predicate)` currently lives in the cold tier, or
    /// `None` if the partition is absent. Placement is observable but not
    /// controllable (SPEC-02 F6).
    ///
    /// Deliberately **not** counted as an access: asking where a partition
    /// lives must not change where [`MemoryTier::rebalance`] puts it next
    /// round.
    pub fn is_cold(&self, graph: GraphId, predicate: TermId) -> Option<bool> {
        self.graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|part| part.is_cold())
    }

    /// Visit the raw bits of every term id **physically present** in this
    /// snapshot: each graph id, each predicate id, and the subject and object
    /// of every row — dead rows included, because a row that is still stored
    /// is still reachable from some pinned reader's version. Backs the mark
    /// phase of the dictionary GC ([`crate::Store::compact`]).
    ///
    /// O(rows). Only ever called right after `compact()`, which has just
    /// walked the same rows.
    pub fn for_each_term_id(&self, mut f: impl FnMut(u64)) {
        for (g, gs) in self.graphs.iter() {
            f(g.0);
            for (p, part) in gs.partitions.iter() {
                f(p.0);
                for (s, o) in part.scan() {
                    f(s.0);
                    f(o.0);
                }
            }
        }
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
    fn partition_is_live(p: &Partition) -> bool {
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

    /// Point-in-time counts and footprint. Bounded by the number of
    /// partitions, except that reading a partition written in batches merges
    /// its runs first (HDB-84) — once, and any other read would pay it too.
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
        // ~16 bytes per physically-retained predicate of overhead. A cold
        // partition's `estimated_bytes()` is its mapped file length instead
        // (SPEC-25 S5) — summed separately into `bytes_cold` so callers can
        // split warm from cold, while `bytes_estimated` keeps counting both.
        let physical_predicates: u64 = self
            .graphs
            .values()
            .map(|g| g.partitions.len() as u64)
            .sum();
        let mut column_bytes = 0u64;
        let mut bytes_cold = 0u64;
        for p in self.graphs.values().flat_map(|g| g.partitions.values()) {
            let b = p.estimated_bytes();
            column_bytes += b;
            if p.is_cold() {
                bytes_cold += b;
            }
        }
        let bytes_estimated = column_bytes + physical_predicates * 16;
        TierStats {
            graphs,
            predicates,
            triples,
            bytes_estimated,
            bytes_cold,
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
    /// Predicates with at least this many live rows materialise the
    /// object-major layout eagerly, at build time; smaller ones materialise it
    /// lazily, on the first object-major read. Two physical layouts serve all
    /// six trie orderings either way (SPEC-02 F4, `crate::ordering`).
    hot_threshold: usize,
    /// version -> number of live pins at that version. Empty ⇒ no pins.
    pins: Arc<Mutex<BTreeMap<u64, usize>>>,
    /// Consecutive rounds each warm partition has gone unread, as counted by
    /// [`Self::rebalance`]. Empty until the first round.
    idle_rounds: Mutex<BTreeMap<(GraphId, TermId), u32>>,
}

impl MemoryTier {
    /// A tier using the process-wide hot-predicate threshold — the
    /// `HORNDB_HOT_THRESHOLD` environment variable if set, else
    /// [`DEFAULT_HOT_THRESHOLD`]. This is what makes the threshold reachable
    /// from a benchmark or a deployment without a code change; every entry
    /// point that builds a store (`Store::in_memory`, `HornBackend::new`, the
    /// bulk loaders) lands here.
    pub fn new() -> Self {
        Self::with_hot_threshold(crate::partition::hot_threshold())
    }

    /// Construct a tier with a custom hot-predicate threshold (SPEC-02 F4).
    pub fn with_hot_threshold(hot_threshold: usize) -> Self {
        Self {
            current: RwLock::new(Arc::new(TierSnapshot::empty())),
            writer: Mutex::new(()),
            hot_threshold,
            pins: Arc::new(Mutex::new(BTreeMap::new())),
            idle_rounds: Mutex::new(BTreeMap::new()),
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

    /// The live version. Bumped by every logical write; compaction leaves it
    /// alone. The dictionary GC uses it to detect a write that landed while it
    /// was marking.
    pub fn version(&self) -> u64 {
        self.current.read().version
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
                // A cold partition holds no retractions, so this also skips it
                // — and `as_warm` below can only be `None` for a cold one.
                if !part.has_retractions() {
                    continue;
                }
                let Some(part) = part.as_warm() else {
                    continue;
                };
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
                    Arc::new(Partition::Warm(
                        builder.build_with_hot_threshold(self.hot_threshold),
                    )),
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
                access: cur.access.clone(),
            });
            *self.current.write() = next;
        }
    }

    /// Encode `(graph, predicate)`'s visible rows into a memory-mapped cold
    /// file under `cold_dir` and swap the cold partition into the live
    /// snapshot (SPEC-25 S5). Returns `false` — and writes nothing — when the
    /// partition is absent, already cold, holds no visible rows, or (after the
    /// compaction pass below) still holds a retraction some pin below the
    /// compaction horizon needs — see the `has_retractions` check inside for
    /// why. That last case is transient: a retry after the pin drops succeeds.
    ///
    /// **Not a logical write.** The new `TierSnapshot` carries the same
    /// version, exactly as [`Self::compact`] does, and a reader that pinned
    /// before the swap keeps its own `Arc<TierSnapshot>` — and with it the
    /// warm partition, unchanged.
    ///
    /// **Why "the set visible now" is enough.** Every read goes through a
    /// pinned `TierSnapshot` and passes that snapshot's own version as `at`;
    /// there is no way to read a snapshot at some other version
    /// ([`crate::Store::snapshot_at`] takes a pin, not a number). A snapshot
    /// holding the cold partition was therefore created at or after this swap,
    /// so its version is `>= ` the version encoded into the file. Every row in
    /// the file was visible then and is still visible now: a retraction would
    /// have promoted the partition back to warm first. So "visible at the
    /// encoded version" and "visible at `at`" name the same set, and the cold
    /// file needs no visibility stamps.
    ///
    /// ponytail: the encode runs under `writer`, the single-writer lock, so a
    /// large partition stalls every other writer for the duration. Upgrade
    /// path, once a placement policy caller exists (HDB-179): encode from a
    /// pinned snapshot outside the lock, then re-check `current.version`
    /// under the lock before swapping in the cold partition.
    pub fn demote(&self, graph: GraphId, predicate: TermId, cold_dir: &Path) -> Result<bool> {
        // Outside the writer lock: `compact()` takes it too, and
        // `parking_lot::Mutex` is not reentrant. SPEC-25 S5 asks demotion to
        // run the compaction pass so dead history is reclaimed rather than
        // dropped silently on the floor by the encoding.
        self.compact();

        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let Some(gs) = cur.graphs.get(&graph) else {
            return Ok(false);
        };
        let Some(warm) = gs.partitions.get(&predicate).and_then(|p| p.as_warm()) else {
            return Ok(false); // absent, or already cold
        };
        if warm.len_at(cur.version) == 0 {
            return Ok(false);
        }
        if warm.has_retractions() {
            // Dead history below `min_pinned` is exactly what `gc_dictionary`
            // marks from (see its doc comment in `store.rs`), so a partition
            // still holding it cannot go cold — the cold encoding would drop
            // those rows, `for_each_term_id` would stop walking them, and a
            // pinned reader below the horizon would resolve their terms as
            // `InvalidTerm` once the dictionary sweep ran. The demotion is
            // only postponed: it succeeds once the pin holding the horizon
            // down drops and a later `compact()` reclaims the dead rows.
            return Ok(false);
        }

        std::fs::create_dir_all(cold_dir)?;
        let path = cold_path(cold_dir, graph, predicate);
        ColdPartition::write(
            &path,
            graph,
            predicate,
            cur.version,
            warm.scan_at(cur.version),
        )?;
        let cold = ColdPartition::open(&path)?;

        let mut partitions = gs.partitions.clone();
        partitions.insert(predicate, Arc::new(Partition::Cold(cold)));
        let mut graphs = cur.graphs.clone();
        graphs.insert(graph, Arc::new(GraphStore { partitions }));
        *self.current.write() = Arc::new(TierSnapshot {
            version: cur.version,
            graphs,
            access: cur.access.clone(),
        });
        Ok(true)
    }

    /// Decode a cold partition back into a warm one and swap it in, deleting
    /// the file. Returns `false` when the partition is absent or already warm.
    /// Not a logical write either — same version, same argument as
    /// [`Self::demote`].
    pub fn promote(&self, graph: GraphId, predicate: TermId) -> Result<bool> {
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let Some(gs) = cur.graphs.get(&graph) else {
            return Ok(false);
        };
        let Some(Partition::Cold(cold)) = gs.partitions.get(&predicate).map(|p| p.as_ref()) else {
            return Ok(false);
        };
        let warm = cold.promote(self.hot_threshold);
        let path = cold.path().to_path_buf();

        let mut partitions = gs.partitions.clone();
        partitions.insert(predicate, Arc::new(Partition::Warm(warm)));
        let mut graphs = cur.graphs.clone();
        graphs.insert(graph, Arc::new(GraphStore { partitions }));
        *self.current.write() = Arc::new(TierSnapshot {
            version: cur.version,
            graphs,
            access: cur.access.clone(),
        });

        // Best effort: a reader pinned before the swap still holds the
        // mapping, and unlinking leaves that inode alive until it drops.
        let _ = std::fs::remove_file(&path);
        Ok(true)
    }

    /// One placement round (SPEC-25 S5). Reads the per-partition access counts
    /// gathered since the previous round and moves partitions between the warm
    /// and cold tiers:
    ///
    /// - warm, at least `cfg.min_rows` visible rows, unread this round → its
    ///   idle count goes up; on reaching `cfg.demote_after_idle_rounds` it is
    ///   demoted, unless `hints.keep_warm` holds it back.
    /// - warm and read this round → idle count back to zero.
    /// - cold and read this round, or named in `hints.keep_warm` → promoted. A
    ///   cold read decodes per call, so a partition that is read at all is
    ///   cheaper warm.
    ///
    /// Hints only ever add to what the statistics decide — keep warm, or pull
    /// warm — and never demote. An empty [`PlacementHints`] therefore gives
    /// exactly the stats-only placement, which is the `ml.enabled = false`
    /// contract in `INTEGRATION-NOTES.md` F4.
    ///
    /// Partitions are visited in `(graph bits, predicate bits)` order, so a
    /// round is deterministic. Not a logical write: `demote` and `promote`
    /// keep the version, and results are unchanged — only cost (SPEC-02 F6).
    ///
    /// ponytail: the idle-count lock is held across a demote's file write.
    /// Only an explicit maintenance caller runs this, so nothing contends for
    /// it; collect the decisions under the lock and act outside it if that
    /// stops being true.
    pub fn rebalance(
        &self,
        cfg: &TieringConfig,
        hints: &PlacementHints,
    ) -> Result<RebalanceReport> {
        let cur = self.current.read().clone();
        let counts: HashMap<(GraphId, TermId), u64> = cur.access.take().into_iter().collect();
        // Walks `graphs` directly rather than through the guarded accessors:
        // deciding placement must not itself count as an access.
        //
        // `live_len()` (O(1)) rather than `len_at(version)` (an O(rows) scan
        // once a partition has any retraction): every partition here is read
        // whether or not it moves, so an O(rows) count would make one
        // rebalance round cost O(total rows in the store) even when nothing
        // moves. `live_len() == len_at(at)` whenever `at` is at or after the
        // version that built the partition object, which `cur.version`
        // always is (copy-on-write hands an older pin a different, earlier
        // object) — see `PredicatePartition::live_len`'s doc comment.
        let mut parts: Vec<(GraphId, TermId, bool, usize)> = cur
            .graphs
            .iter()
            .flat_map(|(g, gs)| {
                gs.partitions
                    .iter()
                    .map(move |(p, part)| (*g, *p, part.is_cold(), part.live_len()))
            })
            .collect();
        parts.sort_unstable_by_key(|(g, p, _, _)| (g.0, p.0));
        drop(cur);

        let mut idle = self.idle_rounds.lock();
        let mut report = RebalanceReport::default();
        for (g, p, is_cold, rows) in parts {
            let key = (g, p);
            let read = counts.get(&key).copied().unwrap_or(0) > 0;
            let keep_warm = hints.keep_warm.contains(&key);
            if is_cold {
                if (read || keep_warm) && self.promote(g, p)? {
                    idle.insert(key, 0);
                    report.promoted.push(key);
                }
            } else if read || keep_warm {
                // `keep_warm` resets the idle count every round it holds,
                // same as an actual read — hints only ever add, so a hint
                // withdrawn last round must not leave a stale idle count
                // behind that demotes on the very next round.
                idle.insert(key, 0);
            } else if rows >= cfg.min_rows {
                let rounds = idle.entry(key).or_insert(0);
                *rounds += 1;
                if *rounds >= cfg.demote_after_idle_rounds && self.demote(g, p, &cfg.cold_dir)? {
                    *rounds = 0;
                    report.demoted.push(key);
                }
            }
        }
        Ok(report)
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

    /// A second pin on the *same* version — an `Arc` clone plus a pin-count
    /// bump. Lets a caller that owns one pin hand out as many equivalent
    /// views as it needs (`Store::snapshot_at`) without re-reading the live
    /// pointer, which would silently move the view to a newer version.
    pub fn repin(&self) -> PinnedSnapshot {
        *self.pins.lock().entry(self.snap.version).or_insert(0) += 1;
        PinnedSnapshot {
            snap: self.snap.clone(),
            pins: self.pins.clone(),
        }
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

impl MemoryTier {
    /// [`Tier::insert_quad_batch`] committing at `version` when given (WAL
    /// replay, SPEC-25 S3) and at `current + 1` otherwise.
    pub(crate) fn insert_at(
        &self,
        quads: &[(GraphId, TermId, TermId, TermId)],
        version: Option<u64>,
    ) -> Result<()> {
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
        // No `copy_forward`: this path no longer carries existing rows forward
        // (HDB-84). The cost it used to cover is now `merge_runs`, emitted
        // once per partition from `PredicatePartition::cols`.
        let mut build_ns = 0u64;
        let mut build_rows = 0u64;

        // Serialize writers so the read-modify-swap is atomic.
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = version.unwrap_or(cur.version + 1);

        // Copy-on-write: clone the top-level graph map (Arc clones of untouched
        // graphs), then rebuild only the affected graphs' partition maps.
        let mut graphs = cur.graphs.clone();
        for (g, pred_rows) in by_graph {
            let mut new_partitions = graphs
                .get(&g)
                .map(|gs| gs.partitions.clone())
                .unwrap_or_default();
            for (p, rows) in pred_rows {
                // New rows: live from this version. The existing rows are
                // shared by `Arc` as an untouched run; only these are sorted
                // here, and the merged view is built once, on the first read
                // (HDB-84) — so `build` covers this batch, not the partition.
                let added = rows.len() as u64;
                let t_build = Instant::now();
                let new_rows: Vec<_> = rows
                    .into_iter()
                    .map(|(s, o)| (s.0, o.0, new_version, UNSET_END))
                    .collect();
                let mut scratch = None;
                let existing = new_partitions
                    .get(&p)
                    .map(|e| warm_for_write(e, &mut scratch, self.hot_threshold));
                let part = match existing {
                    Some(existing) => existing.with_appended_rows(new_rows),
                    None => PartitionBuilder::from_rows(new_rows)
                        .build_with_hot_threshold(self.hot_threshold),
                };
                new_partitions.insert(p, Arc::new(Partition::Warm(part)));
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
            access: cur.access.clone(),
        });
        *self.current.write() = next;

        // Single merge point for the per-partition accumulator.
        record_phase(LoadPhase::Build, Duration::from_nanos(build_ns), build_rows);
        Ok(())
    }

    /// [`Tier::apply_quad_batch`] committing at `version` when given (WAL
    /// replay) and at `current + 1` otherwise. A net-empty batch does not
    /// bump the clock either way.
    pub(crate) fn apply_at(
        &self,
        dels: &[(GraphId, TermId, TermId, TermId)],
        adds: &[(GraphId, TermId, TermId, TermId)],
        version: Option<u64>,
    ) -> Result<ApplyReport> {
        if dels.is_empty() && adds.is_empty() {
            return Ok(ApplyReport::default());
        }
        // Group each side by graph, then predicate, as (s, o) payload pairs.
        //
        // The two sides use different structures because they are asked
        // different questions (HDB-88). The del side is probed once per
        // existing live row, so it stays a `HashSet`. The add side is only
        // ever iterated, so it is a `Vec` that is sorted and deduplicated
        // once, per predicate, at the end of this phase. Sorting does the same
        // in-batch dedupe a `HashSet` did, on a 16-byte element instead of a
        // hash table, and it leaves the pairs in the order the partition
        // builder wants — so the build-time sort sees a sorted run rather than
        // hash order, and the add pass below can walk `still_visible` with a
        // cursor instead of hashing every pair.
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
        let mut add_by_graph: HashMap<GraphId, HashMap<TermId, Vec<(u64, u64)>>> = HashMap::new();
        for &(g, s, p, o) in adds {
            add_by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .push((s.0, o.0));
        }
        for preds in add_by_graph.values_mut() {
            for targets in preds.values_mut() {
                targets.sort_unstable();
                targets.dedup();
            }
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
        let new_version = version.unwrap_or(cur.version + 1);

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
                // A write never lands on a cold partition: promote first
                // (SPEC-25 S5). `scratch` owns the promoted partition for the
                // rest of this predicate's borrow.
                let mut scratch = None;
                let existing: Option<&PredicatePartition> = new_partitions
                    .get(&p)
                    .map(|e| warm_for_write(e, &mut scratch, self.hot_threshold));
                let has_existing = existing.is_some();
                if !has_existing && add_targets.is_none() {
                    // Nothing to retract from (partition doesn't exist) and
                    // nothing to add: a true no-op for this predicate.
                    continue;
                }

                if del_targets.is_none() {
                    // Append-run fast path (HDB-102). No deletion touches this
                    // predicate, so nothing already stored has to move: the
                    // genuinely-new pairs go in as one extra sorted run and the
                    // first read merges them — the design HDB-84 gave
                    // `insert_quad_batch`. N calls into one predicate then cost
                    // O(adds), not O(existing) N times.
                    //
                    // This is decided per *predicate*, not per batch, so a
                    // mixed batch still takes it for every predicate it only
                    // adds to. Only predicates the batch actually deletes from
                    // fall through to the rebuild below.
                    let Some(targets) = add_targets else {
                        // Unreachable: `touched_preds` is the union of the del
                        // and add key sets, so a predicate with no del targets
                        // is in it only because it has add targets.
                        continue;
                    };

                    // `inserted` must stay exact — `Store::insert_quads`
                    // returns it and SPARQL `INSERT DATA` idempotency is
                    // decided by it — so each pair is still tested against what
                    // is already live. `mark_live` answers that with a
                    // galloping search per run and never merges the partition,
                    // which is what keeps this path off the O(existing) curve.
                    let t_merge = Instant::now();
                    let mut already_live = vec![false; targets.len()];
                    if let Some(existing) = existing {
                        existing.mark_live(targets, &mut already_live);
                    }
                    let new_rows: Vec<_> = targets
                        .iter()
                        .zip(&already_live)
                        .filter(|(_, live)| !**live)
                        .map(|(&(s, o), _)| (s, o, new_version, UNSET_END))
                        .collect();
                    merge_ns += t_merge.elapsed().as_nanos() as u64;
                    merge_rows += targets.len() as u64;

                    if new_rows.is_empty() {
                        // Every pair was already visible: a counted no-op that
                        // must not add an empty run (it would consume a slot
                        // against `MAX_RUNS` for nothing).
                        continue;
                    }
                    inserted += new_rows.len();

                    let added = new_rows.len() as u64;
                    let t_build = Instant::now();
                    let part = match existing {
                        Some(existing) => existing.with_appended_rows(new_rows),
                        None => PartitionBuilder::from_rows(new_rows)
                            .build_with_hot_threshold(self.hot_threshold),
                    };
                    new_partitions.insert(p, Arc::new(Partition::Warm(part)));
                    build_ns += t_build.elapsed().as_nanos() as u64;
                    build_rows += added;
                    continue;
                }

                // Rebuild path: this predicate has deletions, and a deletion
                // end-stamps a row *inside* an existing run. Runs are immutable
                // and shared by `Arc` with the snapshots older readers pinned,
                // so a stamp cannot be written in place — the partition is
                // carried forward into a fresh builder instead. See the HDB-102
                // note in `crates/storage/INTEGRATION-NOTES.md`.
                //
                // One pass over the existing rows: end-stamp del matches
                // (dels apply before adds), carry every row forward, and
                // track which (s, o) pairs remain visible so the add pass
                // below knows what is genuinely new.
                let mut builder = PartitionBuilder::default();
                // Live pairs surviving the dels, in the partition's own SPO
                // order. A sorted `Vec` rather than a `HashSet` (HDB-88): the
                // rows arrive sorted, so this is a push per row instead of a
                // hash insert, and the add pass reads it with one merge cursor.
                let mut still_visible: Vec<(u64, u64)> = Vec::new();
                let mut carried = 0u64;
                let t_copy = Instant::now();
                if let Some(existing) = existing {
                    let n = existing.len();
                    carried = n as u64;
                    // The row count is known here, and at most one live pair
                    // per row lands in `still_visible`, so one allocation
                    // replaces log2(n) doublings per predicate per batch.
                    still_visible.reserve_exact(n);
                    for i in 0..n {
                        let s = existing.subjects().value(i);
                        let o = existing.objects().value(i);
                        let begin = existing.begins().value(i);
                        let mut end = existing.ends().value(i);
                        if end == UNSET_END {
                            if del_targets.is_some_and(|t| t.contains(&(s, o))) {
                                end = new_version;
                                retracted += 1;
                            } else if still_visible.last() != Some(&(s, o)) {
                                // `Columns` holds at most one live row per
                                // (s, o) and stores rows in (s, o, begin)
                                // order, so skipping an equal predecessor
                                // keeps this strictly ascending.
                                still_visible.push((s, o));
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
                    // Both lists ascend, so one cursor over `still_visible`
                    // replaces a hash probe per added pair. That is O(live
                    // rows + adds) for the whole predicate — the merge bound,
                    // and never worse than the copy-forward pass just above,
                    // which is already O(live rows). A binary search would beat
                    // it when a handful of adds land in a huge partition and
                    // lose when the two lists interleave densely; the linear
                    // cursor is the one that cannot be pathological.
                    //
                    // Should either list ever arrive out of order the cursor
                    // can miss a match and append a row that is already live;
                    // `Columns::sort_dedup` then collapses the pair back to its
                    // earlier `begin`, so the stored data stays correct and
                    // only `inserted` over-counts.
                    let mut vis = 0usize;
                    for &(s, o) in targets {
                        while still_visible.get(vis).is_some_and(|v| *v < (s, o)) {
                            vis += 1;
                        }
                        // Visible "after the dels" — a quad ended above by
                        // this same batch is not in `still_visible`, so a
                        // del+add of the same quad within one batch counts
                        // as both a retract and a fresh insert.
                        if still_visible.get(vis) != Some(&(s, o)) {
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
                    Arc::new(Partition::Warm(
                        builder.build_with_hot_threshold(self.hot_threshold),
                    )),
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
                access: cur.access.clone(),
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

    /// Force the live version to `version` without a logical write — the
    /// WAL checkpoint replay restores the commit clock with it. No-op when
    /// already there.
    pub(crate) fn set_version(&self, version: u64) {
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        if cur.version != version {
            *self.current.write() = Arc::new(TierSnapshot {
                version,
                graphs: cur.graphs.clone(),
                access: cur.access.clone(),
            });
        }
    }
}

impl TierWrite for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        self.insert_at(quads, None)
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
                let mut scratch = None;
                let Some(existing) = new_partitions
                    .get(&p)
                    .map(|e| warm_for_write(e, &mut scratch, self.hot_threshold))
                else {
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
                    Arc::new(Partition::Warm(
                        builder.build_with_hot_threshold(self.hot_threshold),
                    )),
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
                access: cur.access.clone(),
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
        self.apply_at(dels, adds, None)
    }
}

impl Tier for MemoryTier {
    fn predicate(&self, _graph: GraphId, _predicate: TermId) -> Option<&Partition> {
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
        F: FnOnce(&Partition) -> R,
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
    use crate::tier::{Tier, TierWrite};

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
                    let p = p.as_warm().expect("insert-only tier stays warm");
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

    /// Retract and re-insert against a partition that is still a list of
    /// unmerged runs: the retraction has to see rows spread across 13 runs,
    /// and re-inserting a retracted quad has to make it live again. Asserts
    /// the live count after each step.
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

    /// The run cap (HDB-84 `MAX_RUNS`) forces a merge mid-write so neither the
    /// run-list clone nor the per-run overhead grows without bound. Drive it
    /// the way that reaches it — one quad per call, no read in between — and
    /// check the store is still exactly what one call would have produced.
    #[test]
    fn run_cap_forces_a_merge_without_changing_contents() {
        use crate::ordering::Ordering;
        use crate::partition::MAX_RUNS;

        let pred = id(100);
        let n = (MAX_RUNS + 8) as u64;
        let quads: Vec<_> = (0..n)
            .map(|i| (DEFAULT_GRAPH, id(i % 997), pred, id(i % 31)))
            .collect();

        let one = MemoryTier::new();
        one.insert_quad_batch(&quads).unwrap();

        let many = MemoryTier::new();
        for q in &quads {
            many.insert_quad_batch(std::slice::from_ref(q)).unwrap();
        }

        // The cap must have fired, and this is the only assertion that sees
        // it: read the run count *before* anything else, because every read
        // path collapses the runs. Insert k leaves k runs until insert 4,096
        // trips the cap and merges to 1; the last 8 inserts then rebuild to 9.
        // Without the cap this would be all 4,104.
        let runs = many
            .snapshot()
            .with_predicate(DEFAULT_GRAPH, pred, |p| {
                p.as_warm()
                    .expect("insert-only tier stays warm")
                    .run_count()
            })
            .unwrap();
        assert_eq!(runs, 9, "cap did not fire: {runs} runs after {n} inserts");

        let (a, b) = (one.snapshot(), many.snapshot());
        let read = |snap: &crate::memory_tier::TierSnapshot| {
            snap.with_predicate(DEFAULT_GRAPH, pred, |p| {
                (
                    p.len(),
                    p.live_len(),
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
        assert_eq!(read(&a), read(&b));
        assert_eq!(a.triple_count(), b.triple_count());
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
    use crate::tier::TierWrite;

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
    fn in_batch_duplicate_adds_are_counted_once() {
        // The add side is a sorted `Vec`, not a `HashSet` (HDB-88), so the
        // in-batch dedupe it used to get for free now comes from the sort.
        let tier = MemoryTier::new();
        let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
        let other = (DEFAULT_GRAPH, id(3), id(100), id(4));

        let report = tier
            .apply_quad_batch(&[], &[q, other, q, q, other])
            .unwrap();
        assert_eq!(report.inserted, 2, "duplicates within one batch collapse");
        assert_eq!(report.retracted, 0);

        let snap = tier.snapshot();
        let pairs = snap
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(snap.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(pairs, vec![(id(1), id(2)), (id(3), id(4))]);
    }

    #[test]
    fn adds_are_stored_sorted_whatever_order_they_arrive_in() {
        // The group phase sorts the add list, so the builder sees a sorted run
        // and the partition comes out in SPO order regardless of input order.
        let tier = MemoryTier::new();
        let quads: Vec<_> = [7u64, 2, 9, 1, 5, 3]
            .into_iter()
            .map(|n| (DEFAULT_GRAPH, id(n), id(100), id(n * 10)))
            .collect();
        let report = tier.apply_quad_batch(&[], &quads).unwrap();
        assert_eq!(report.inserted, 6);

        let snap = tier.snapshot();
        let pairs = snap
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(snap.version()).collect::<Vec<_>>()
            })
            .unwrap();
        let mut expected: Vec<_> = [1u64, 2, 3, 5, 7, 9]
            .into_iter()
            .map(|n| (id(n), id(n * 10)))
            .collect();
        expected.sort_by_key(|(s, _)| s.0);
        assert_eq!(pairs, expected);
    }

    #[test]
    fn partly_live_adds_over_an_existing_partition() {
        // Exercises the `still_visible` merge cursor: some adds are already
        // live, some are new, one is retracted-and-re-added in the same batch,
        // and they interleave in sort order.
        let tier = MemoryTier::new();
        let base: Vec<_> = (1u64..=6)
            .map(|n| (DEFAULT_GRAPH, id(n), id(100), id(n * 10)))
            .collect();
        tier.insert_quad_batch(&base).unwrap(); // v1

        // Retract (2, 20) and (5, 50); re-add (2, 20) in the same batch.
        let dels = vec![base[1], base[4]];
        // Adds, deliberately out of order: two already live, one re-added
        // after its own del, two genuinely new.
        let adds = vec![
            base[3],                                 // (4, 40), already live
            (DEFAULT_GRAPH, id(8), id(100), id(80)), // new, sorts last
            base[1],                                 // (2, 20), del'd above
            (DEFAULT_GRAPH, id(0), id(100), id(0)),  // new, sorts first
            base[0],                                 // (1, 10), already live
        ];
        let report = tier.apply_quad_batch(&dels, &adds).unwrap();
        assert_eq!(report.retracted, 2);
        assert_eq!(
            report.inserted, 3,
            "the two already-live adds are no-ops; the re-added quad is fresh"
        );

        let snap = tier.snapshot();
        let pairs = snap
            .with_predicate(DEFAULT_GRAPH, id(100), |p| {
                p.scan_at(snap.version()).collect::<Vec<_>>()
            })
            .unwrap();
        let expected: Vec<_> = [0u64, 1, 2, 3, 4, 6, 8]
            .into_iter()
            .map(|n| (id(n), id(n * 10)))
            .collect();
        assert_eq!(pairs, expected, "(5, 50) gone, (2, 20) back, 0 and 8 new");
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
