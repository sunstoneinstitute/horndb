---
status: executed
date: 2026-06-15
scope: "Copy-on-Write Snapshot Isolation"
---

# Copy-on-Write Snapshot Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `horndb-storage` copy-on-write snapshot isolation so concurrent readers see a stable, consistent view of the store while a single writer appends (SPEC-02 increment #19, parent epic #3).

**Architecture:** Replace `MemoryTier`'s single mutable `RwLock<Inner>` with an immutable, versioned `Arc<TierSnapshot>` held behind a pointer (`RwLock<Arc<TierSnapshot>>`) plus a writer-serialization `Mutex`. A `TierSnapshot` owns `HashMap<GraphId, Arc<GraphStore>>`, and each `GraphStore` owns `HashMap<TermId, Arc<PredicatePartition>>`. Writes copy-on-write: clone the top-level graph map (cheap `Arc` clones), rebuild only the affected graph's partition map (replaying the old partition's pairs into the new builder, as today), bump the version, and atomically swap the pointer. Readers `clone()` the current `Arc<TierSnapshot>` once; that `Arc` pins their view — later writes allocate a new snapshot and never mutate the pinned one, and the old snapshot stays alive (and readable) until the last reader drops it. A public `Store::snapshot()` returns a `StoreSnapshot` that exposes the existing read methods against the pinned tier state, and the live `Store` read methods are refactored to read through a freshly-pinned snapshot so they stay consistent within a single call.

**Tech Stack:** Rust 1.90, `parking_lot::{RwLock, Mutex}` (already a workspace dep — no new crates), `std::sync::Arc`, Arrow `UInt64Array` columns, `roaring` bitmaps.

---

## Background / current state

- `crates/storage/src/memory_tier.rs` — `MemoryTier { inner: RwLock<Inner>, hot_threshold }`, `Inner { graphs: HashMap<GraphId, GraphStore> }`, `GraphStore { partitions: HashMap<TermId, PredicatePartition> }`. `insert_quad_batch` takes the write lock and rebuilds each affected partition in place (`remove` + replay old pairs + `insert` rebuilt). Reads (`with_predicate`, `ordered_predicate`, `predicates`, `top_predicates`, `triple_count`, `stats`, `graphs`) take the read lock.
- `crates/storage/src/store.rs` — `Store { dictionary, tier: Box<dyn Tier> }`. Read methods downcast `tier` to `MemoryTier` and call its accessors, then materialize `TermId`s back to `oxrdf::Term`s via the dictionary. `scan_all_term_ids` iterates `tier.predicates(g)` and then calls `with_predicate` per predicate — **two separate locked reads, not internally consistent under concurrent writes** today.
- `crates/storage/src/partition.rs` — `PredicatePartition` is immutable once built (only interior mutability is an internally-synchronised `OnceLock<ObjectMajor>` lazy layout cache, which is safe to share across snapshots through an `Arc`). Public methods used here: `scan() -> impl Iterator<Item=(TermId, TermId)>`, `ordered(Ordering) -> OrderedColumns`, `len()`, `estimated_bytes()`.
- `crates/storage/src/snapshot/mod.rs` — `export_snapshot(store, w)` calls `store.has_named_graph_data()` then `store.scan_all_term_ids()`. Making `scan_all_term_ids` read one pinned snapshot makes a checkpoint internally consistent (NF5 tie-in).
- `crates/storage/src/tier.rs` — `Tier` trait (`insert_quad_batch`, `predicate`, `predicates`, `graphs`, `triple_count`, `stats`, `as_any`). `predicate()` is a Stage-1 stub returning `None`; keep it.

`PredicatePartition` is `Send + Sync` (Arrow `Arc<UInt64Array>` + `RoaringTreemap` + `OnceLock<ObjectMajor>` are all `Sync`), so `Arc<PredicatePartition>` / `Arc<GraphStore>` / `Arc<TierSnapshot>` are all `Send + Sync` — required because `MemoryTier: Send + Sync` via the `Tier` trait bound.

---

## File Structure

- **Modify** `crates/storage/src/memory_tier.rs` — introduce `TierSnapshot` (immutable, versioned) and `Arc`-wrapped `GraphStore`/partitions; rewrite `MemoryTier` to hold `RwLock<Arc<TierSnapshot>>` + writer `Mutex`; move read accessors onto `TierSnapshot`; rewrite `insert_quad_batch` as CoW; add `MemoryTier::snapshot()`. Update the in-module tests' field expectations only where types changed (the public assertions are unchanged).
- **Modify** `crates/storage/src/store.rs` — add `StoreSnapshot<'a>` + `Store::snapshot()`; refactor `scan_predicate_default_graph`, `scan_predicate_ordered`, `top_predicates`, `scan_all_term_ids`, `triple_count`, `stats` to read through a pinned snapshot (shared materialization helpers).
- **Modify** `crates/storage/src/lib.rs` — re-export `StoreSnapshot` (and `TierSnapshot` if made public) so downstream crates and tests can name the snapshot type.
- **Create** `crates/storage/tests/snapshot_isolation.rs` — concurrent-read / single-writer integration test demonstrating snapshot stability under interleaved writes, plus a checkpoint-consistency (NF5) test.
- **Modify** `crates/storage/INTEGRATION-NOTES.md` and/or `STAGE1-ACCEPTANCE.md` — record that #19 (CoW snapshot isolation) is delivered and how the model works.
- **Modify** `docs/architecture.md` — flip the SPEC-02 snapshot-isolation/MVCC Status field from planned/specified → implemented (CoW snapshots; true per-tuple MVCC still deferred).

No public method renames of existing APIs; only **additions** (`Store::snapshot`, `StoreSnapshot`, `MemoryTier::snapshot`, `TierSnapshot`) and internal refactors. Existing call sites (sparql, harness) keep compiling unchanged.

---

## Task 1: Introduce the immutable `TierSnapshot` with `Arc`-shared graphs/partitions

**Files:**
- Modify: `crates/storage/src/memory_tier.rs`

This task changes the internal representation and CoW write path, keeping all existing public method signatures on `MemoryTier` working (they delegate to the snapshot). The in-module tests at the bottom of the file must still pass unchanged.

- [ ] **Step 1: Write a failing in-module test for snapshot pinning + versioning**

Add to the `#[cfg(test)] mod tests` block in `crates/storage/src/memory_tier.rs`:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p horndb-storage --lib memory_tier 2>&1 | tail -20`
Expected: FAIL — `no method named 'snapshot' found for struct 'MemoryTier'` / `no method 'version'`.

- [ ] **Step 3: Implement `TierSnapshot`, `Arc`-shared stores, CoW writes, and `snapshot()`**

Replace the type definitions and `Tier` impl in `crates/storage/src/memory_tier.rs`. Full new content for the non-test portion of the file:

```rust
//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition, DEFAULT_HOT_THRESHOLD};
use crate::term::{GraphId, TermId};
use crate::tier::{Tier, TierStats};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
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
            .map(|p| p.len() as u64)
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
                    .map(|(p, part)| (*p, part.len() as u64))
                    .collect()
            })
            .unwrap_or_default();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0 .0.cmp(&b.0 .0)));
        counts.truncate(n);
        counts
    }

    pub fn stats(&self) -> TierStats {
        let graphs = self.graphs.len() as u64;
        let predicates: u64 = self.graphs.values().map(|g| g.partitions.len() as u64).sum();
        let triples: u64 = self
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len() as u64)
            .sum();
        // Per-partition column footprint (16 B/row, doubled when the
        // object-major layout is materialised for a hot predicate); plus
        // ~16 bytes/predicate overhead.
        let column_bytes: u64 = self
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.estimated_bytes())
            .sum();
        let bytes_estimated = column_bytes + predicates * 16;
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
        }
    }

    /// The hot-predicate triple-count threshold in effect for this tier.
    pub fn hot_threshold(&self) -> usize {
        self.hot_threshold
    }

    /// Pin the current immutable tier state. The returned `Arc` isolates the
    /// caller from later writes (copy-on-write): subsequent `insert_quad_batch`
    /// calls allocate a new snapshot and never mutate this one.
    pub fn snapshot(&self) -> Arc<TierSnapshot> {
        self.current.read().clone()
    }
}

impl Default for MemoryTier {
    fn default() -> Self {
        Self::new()
    }
}

impl Tier for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        if quads.is_empty() {
            return Ok(());
        }
        // Group incoming pairs by graph, then predicate, into builders.
        let mut by_graph: HashMap<GraphId, HashMap<TermId, PartitionBuilder>> = HashMap::new();
        for &(g, s, p, o) in quads {
            by_graph
                .entry(g)
                .or_default()
                .entry(p)
                .or_default()
                .append(s, o);
        }

        // Serialize writers so the read-modify-swap is atomic.
        let _w = self.writer.lock();
        let cur = self.current.read().clone();

        // Copy-on-write: clone the top-level graph map (Arc clones of untouched
        // graphs), then rebuild only the affected graphs' partition maps.
        let mut graphs = cur.graphs.clone();
        for (g, pred_builders) in by_graph {
            let mut new_partitions = graphs
                .get(&g)
                .map(|gs| gs.partitions.clone())
                .unwrap_or_default();
            for (p, mut builder) in pred_builders {
                if let Some(existing) = new_partitions.get(&p) {
                    for (s, o) in existing.scan() {
                        builder.append(s, o);
                    }
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
            version: cur.version + 1,
            graphs,
        });
        *self.current.write() = next;
        Ok(())
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

    /// The top-`n` predicates in `graph` by triple count in the current
    /// snapshot, descending (deterministic tie-break by predicate id).
    pub fn top_predicates(&self, graph: GraphId, n: usize) -> Vec<(TermId, u64)> {
        self.snapshot().top_predicates(graph, n)
    }
}
```

Note: the existing in-module tests call `tier.with_predicate(...)`, `tier.triple_count()`, `tier.predicates(...)`, `tier.insert_quad_batch(...)` — all preserved. The two new tests from Step 1 call `tier.snapshot()` and `TierSnapshot::{version,triple_count}` — now defined.

- [ ] **Step 4: Run the storage lib tests**

Run: `cargo test -p horndb-storage --lib 2>&1 | tail -25`
Expected: PASS — all existing `memory_tier::tests` plus the two new ones green.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/memory_tier.rs
git commit -m "feat(storage): immutable versioned TierSnapshot with copy-on-write writes (SPEC-02 #19)"
```

---

## Task 2: Public `Store::snapshot()` / `StoreSnapshot` read API

**Files:**
- Modify: `crates/storage/src/store.rs`
- Modify: `crates/storage/src/lib.rs`

Expose a pinned read transaction at the `Store` level and route the live read methods through it so a single call is internally consistent.

- [ ] **Step 1: Write a failing in-module test for `Store::snapshot()` stability**

Add to the `#[cfg(test)] mod tests` block in `crates/storage/src/store.rs`:

```rust
    #[test]
    fn store_snapshot_is_stable_across_writes() {
        let store = Store::in_memory();
        store
            .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
            .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.version(), 1);
        assert_eq!(snap.triple_count(), 1);

        // Mutate the live store; the pinned snapshot is unaffected.
        store
            .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/c"))])
            .unwrap();
        assert_eq!(snap.triple_count(), 1);
        assert_eq!(
            snap.scan_predicate_default_graph(&iri("http://ex/p"))
                .unwrap()
                .len(),
            1
        );

        // The live store sees both triples.
        assert_eq!(store.triple_count(), 2);
        assert_eq!(
            store
                .scan_predicate_default_graph(&iri("http://ex/p"))
                .unwrap()
                .len(),
            2
        );
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo test -p horndb-storage --lib store:: 2>&1 | tail -20`
Expected: FAIL — `no method named 'snapshot' found for struct 'Store'`.

- [ ] **Step 3: Add `StoreSnapshot` and route reads through it**

In `crates/storage/src/store.rs`, add imports and the `StoreSnapshot` type, plus `Store::snapshot()`. Replace the bodies of the read methods (`scan_predicate_default_graph`, `scan_predicate_ordered`, `top_predicates`, `scan_all_term_ids`) to delegate to a pinned snapshot. Keep `triple_count`/`stats` reading the live tier (already a single tier call).

Add near the top, after the existing `use` lines:

```rust
use crate::memory_tier::TierSnapshot;
use std::sync::Arc;
```

Add the `Store::snapshot()` method (inside `impl Store`, e.g. after `stats`):

```rust
    /// Begin a read transaction: pin a stable, internally-consistent snapshot of
    /// the store (SPEC-02 copy-on-write snapshots — the Stage-1 substitute for
    /// per-tuple MVCC). Concurrent writers append to a new snapshot and never
    /// disturb the pinned view; it stays readable until dropped. The dictionary
    /// is append-only, so term ids in the pinned view never change meaning even
    /// as new terms are interned by other transactions.
    pub fn snapshot(&self) -> StoreSnapshot<'_> {
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        StoreSnapshot {
            tier: mt.snapshot(),
            dictionary: &self.dictionary,
        }
    }
```

Replace the four read-method bodies so they pin once and delegate:

```rust
    pub fn scan_predicate_default_graph(&self, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        self.snapshot().scan_predicate_default_graph(predicate)
    }

    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        self.snapshot().scan_predicate_ordered(predicate, ord)
    }

    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        self.snapshot().top_predicates(n)
    }

    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        self.snapshot().scan_all_term_ids()
    }
```

Add the `StoreSnapshot` type and its inherent methods at the end of the file, before `#[cfg(test)]`. The method bodies are the materialization logic moved verbatim from the old `Store` methods, but reading from `self.tier` (an `Arc<TierSnapshot>`) instead of downcasting:

```rust
/// A pinned, internally-consistent read view of a [`Store`] (SPEC-02
/// copy-on-write snapshot). Holds an `Arc` to the immutable tier state captured
/// at [`Store::snapshot`] time plus a borrow of the append-only dictionary for
/// term materialization. Cheap to create; cheap to drop.
pub struct StoreSnapshot<'a> {
    tier: Arc<TierSnapshot>,
    dictionary: &'a Dictionary,
}

impl StoreSnapshot<'_> {
    /// The snapshot id (monotonic tier version) this view is pinned to.
    pub fn version(&self) -> u64 {
        self.tier.version()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Scan a single predicate in the default graph, returning materialized
    /// (subject, object) `Term` pairs.
    pub fn scan_predicate_default_graph(&self, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        let p_id = self.dictionary.intern(predicate)?;
        let pairs = self
            .tier
            .with_predicate(DEFAULT_GRAPH, p_id, |part| part.scan().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(pairs.len());
        for (s_id, o_id) in pairs {
            out.push((self.term(s_id)?, self.term(o_id)?));
        }
        Ok(out)
    }

    /// Scan a single predicate in the default graph in the requested index
    /// ordering (SPEC-02 F4), returning materialized `(s, p, o)` triples.
    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        let p_id = self.dictionary.intern(predicate)?;
        let cols = match self.tier.ordered_predicate(DEFAULT_GRAPH, p_id, ord) {
            Some(cols) => cols,
            None => return Ok(Vec::new()),
        };
        let mut out = Vec::with_capacity(cols.len());
        for (s_id, o_id) in cols.subject_object() {
            out.push((self.term(s_id)?, predicate.clone(), self.term(o_id)?));
        }
        Ok(out)
    }

    /// The top-`n` predicates in the default graph by triple count (descending).
    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        let top = self.tier.top_predicates(DEFAULT_GRAPH, n);
        let mut out = Vec::with_capacity(top.len());
        for (p_id, count) in top {
            out.push((self.term(p_id)?, count));
        }
        Ok(out)
    }

    /// Dump every default-graph triple as raw `TermId`s, in arbitrary order,
    /// from this single pinned snapshot (so the dump is internally consistent
    /// even under concurrent writes — the NF5 checkpoint-consistency property).
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        let mut out = Vec::with_capacity(self.tier.triple_count() as usize);
        for p_id in self.tier.predicates(DEFAULT_GRAPH) {
            self.tier.with_predicate(DEFAULT_GRAPH, p_id, |part| {
                out.extend(part.scan().map(|(s, o)| (s, p_id, o)));
            });
        }
        out
    }

    fn term(&self, id: TermId) -> Result<Term> {
        self.dictionary
            .lookup(id)
            .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {id:?}")))
    }
}
```

Delete the now-unused `use crate::memory_tier::MemoryTier;` only if it becomes unused — `Store::snapshot()` still downcasts to `MemoryTier`, so keep that import. The old method bodies that downcast directly are gone (replaced by the delegating versions above); confirm no `downcast_ref::<MemoryTier>()` remains except inside `Store::snapshot()`.

- [ ] **Step 4: Export `StoreSnapshot` from the crate**

In `crates/storage/src/lib.rs`, add `StoreSnapshot` (and `TierSnapshot`) to the public re-exports next to the existing `Store` re-export. Find the line re-exporting `Store` (e.g. `pub use store::Store;` or a grouped `pub use store::{...};`) and extend it:

```rust
pub use store::{FootprintReport, Store, StoreSnapshot};
```

If `memory_tier` is re-exported, add `TierSnapshot`:

```rust
pub use memory_tier::{MemoryTier, TierSnapshot};
```

(Match the existing re-export style in `lib.rs`; only add the new names. If `MemoryTier` is not currently re-exported, re-export just `TierSnapshot` via `pub use memory_tier::TierSnapshot;`, since `StoreSnapshot::version()` returns it transitively but callers mainly use `StoreSnapshot`.)

- [ ] **Step 5: Run the storage lib + doc build**

Run: `cargo test -p horndb-storage --lib 2>&1 | tail -25`
Expected: PASS — including `store::tests::store_snapshot_is_stable_across_writes`.

Run: `cargo build -p horndb-storage 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/storage/src/store.rs crates/storage/src/lib.rs
git commit -m "feat(storage): Store::snapshot() pinned read transactions (SPEC-02 #19)"
```

---

## Task 3: Concurrent-read / single-writer isolation integration test

**Files:**
- Create: `crates/storage/tests/snapshot_isolation.rs`

This is the issue's primary "done-when": demonstrate snapshot stability under interleaved writes.

- [ ] **Step 1: Write the integration test**

Create `crates/storage/tests/snapshot_isolation.rs`:

```rust
//! SPEC-02 #19 — copy-on-write snapshot isolation: concurrent readers see a
//! stable, consistent view while a single writer appends.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::sync::Arc;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn p() -> Term {
    iri("http://ex/p")
}

fn subj(i: u64) -> Term {
    iri(&format!("http://ex/s{i}"))
}

/// A reader that pins a snapshot sees a fixed triple count for the snapshot's
/// whole lifetime, regardless of how many triples the writer appends meanwhile.
#[test]
fn reader_pinned_snapshot_is_stable_under_concurrent_writes() {
    let store = Arc::new(Store::in_memory());

    // Seed 100 triples on predicate p.
    let seed: Vec<(Term, Term, Term)> = (0..100).map(|i| (subj(i), p(), iri("http://ex/o"))).collect();
    store.insert_triples(&seed).unwrap();

    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            // Append 1000 more triples, one batch at a time, to maximise the
            // chance of interleaving with the readers below.
            for i in 100..1100 {
                store
                    .insert_triples(&[(subj(i), p(), iri("http://ex/o"))])
                    .unwrap();
            }
        })
    };

    // Spawn readers that each pin a snapshot and repeatedly verify the count it
    // reports never changes for that snapshot's lifetime.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let store = Arc::clone(&store);
        readers.push(std::thread::spawn(move || {
            let snap = store.snapshot();
            let pinned_version = snap.version();
            let pinned_count = snap.triple_count();
            assert!(pinned_count >= 100, "snapshot must see at least the seed");
            for _ in 0..2000 {
                assert_eq!(
                    snap.triple_count(),
                    pinned_count,
                    "pinned snapshot triple count drifted under concurrent writes"
                );
                assert_eq!(snap.version(), pinned_version);
                // The materialized scan must match the count for the same view.
                let rows = snap.scan_predicate_default_graph(&p()).unwrap();
                assert_eq!(rows.len() as u64, pinned_count);
            }
        }));
    }

    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }

    // After all writes, a fresh snapshot sees everything.
    let final_snap = store.snapshot();
    assert_eq!(final_snap.triple_count(), 1100);
    assert_eq!(final_snap.version(), 1101); // 1 seed batch + 1000 single-triple batches
}

/// Two snapshots pinned at different times reflect their respective versions;
/// the older one is never disturbed by the writes that produced the newer one.
#[test]
fn older_snapshot_outlives_newer_writes() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(subj(0), p(), iri("http://ex/o"))])
        .unwrap();
    let early = store.snapshot();

    for i in 1..50 {
        store
            .insert_triples(&[(subj(i), p(), iri("http://ex/o"))])
            .unwrap();
    }
    let late = store.snapshot();

    assert_eq!(early.triple_count(), 1);
    assert_eq!(late.triple_count(), 50);
    assert!(late.version() > early.version());

    // Dropping the late snapshot does not affect the early one (no shared
    // mutable state); the early view is still its original size.
    drop(late);
    assert_eq!(early.triple_count(), 1);
    assert_eq!(early.scan_predicate_default_graph(&p()).unwrap().len(), 1);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-storage --test snapshot_isolation 2>&1 | tail -20`
Expected: PASS — both tests green. (If the final-version assertion `1101` is off, it indicates batches were coalesced unexpectedly; the seed is one batch + 1000 single-triple batches = 1001 writes from version 0, so version 1001 — **recompute before adjusting:** version starts at 0, seed → 1, then 1000 batches → 1001. Fix the literal to `1001` and `final triple count` stays `1100`. Verify against the actual run output and set the literal to the observed value, which must equal `1 + 1000 = 1001`.)

- [ ] **Step 3: Correct the version literal if needed and re-run**

If Step 2 reported `assertion failed: left: 1001, right: 1101`, change `assert_eq!(final_snap.version(), 1101)` to `assert_eq!(final_snap.version(), 1001)` (seed batch = v1, then 1000 single batches = v1001) and re-run:

Run: `cargo test -p horndb-storage --test snapshot_isolation 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 4: Run under the thread sanitizer-ish stress (loom is not set up; use repeated runs)**

Run: `for i in 1 2 3 4 5; do cargo test -p horndb-storage --test snapshot_isolation 2>&1 | tail -3; done`
Expected: PASS on every repetition (catches obvious races in the pointer swap / Arc handling).

- [ ] **Step 5: Commit**

```bash
git add crates/storage/tests/snapshot_isolation.rs
git commit -m "test(storage): concurrent-read/single-writer snapshot isolation (SPEC-02 #19)"
```

---

## Task 4: NF5 checkpoint-consistency test (snapshot-consistent export)

**Files:**
- Create: `crates/storage/tests/snapshot_isolation.rs` (append) **or** add to the existing test file from Task 3.

The HDT export already routes through `scan_all_term_ids`, which now reads one pinned snapshot — so a checkpoint taken concurrently with writes is internally consistent (NF5: "clean restart → last checkpointed state"). Pin that behaviour with a test.

- [ ] **Step 1: Add the checkpoint-consistency test**

Append to `crates/storage/tests/snapshot_isolation.rs`:

```rust
/// A checkpoint (HDT export) taken while a writer is appending must be
/// internally consistent: the exported snapshot round-trips to a store whose
/// triple set is exactly some committed prefix — never a torn mix where the
/// dictionary and triples disagree. We assert the strongest available property:
/// export → import yields a triple count that is itself a valid committed
/// count, and the round-trip is loss-free for whatever was captured.
#[test]
fn checkpoint_export_is_internally_consistent_under_writes() {
    use std::io::Cursor;

    let store = Arc::new(Store::in_memory());
    store
        .insert_triples(
            &(0..200)
                .map(|i| (subj(i), p(), iri("http://ex/o")))
                .collect::<Vec<_>>(),
        )
        .unwrap();

    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            for i in 200..700 {
                store
                    .insert_triples(&[(subj(i), p(), iri("http://ex/o"))])
                    .unwrap();
            }
        })
    };

    // Take several checkpoints while the writer runs.
    for _ in 0..20 {
        let mut buf = Vec::new();
        let stats = store.export_snapshot(&mut buf).unwrap();
        let reimported = horndb_storage::snapshot::import_snapshot(&mut Cursor::new(&buf)).unwrap();
        // Loss-free round trip for the captured checkpoint.
        assert_eq!(reimported.triple_count(), stats.triples);
        // The captured count is between the seed and the final totals.
        assert!((200..=700).contains(&stats.triples));
    }

    writer.join().unwrap();

    // Final checkpoint captures everything and round-trips exactly.
    let mut buf = Vec::new();
    let stats = store.export_snapshot(&mut buf).unwrap();
    assert_eq!(stats.triples, 700);
    let reimported =
        horndb_storage::snapshot::import_snapshot(&mut std::io::Cursor::new(&buf)).unwrap();
    assert_eq!(reimported.triple_count(), 700);
}
```

If `horndb_storage::snapshot::import_snapshot` is not publicly reachable, check `crates/storage/src/lib.rs` for the `snapshot` module visibility; it is `pub mod snapshot` (confirmed in `snapshot/mod.rs` exports `pub fn import_snapshot`). If the path differs, adjust to the exported path (e.g. `horndb_storage::import_snapshot` if re-exported at the crate root).

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-storage --test snapshot_isolation checkpoint 2>&1 | tail -20`
Expected: PASS. If `export_snapshot` errors with the named-graph guard, it means the test accidentally wrote to a named graph — it does not (default graph only), so this should not occur.

- [ ] **Step 3: Commit**

```bash
git add crates/storage/tests/snapshot_isolation.rs
git commit -m "test(storage): checkpoint export is snapshot-consistent under writes (SPEC-02 NF5 #19)"
```

---

## Task 5: Docs sync (architecture Status + crate notes)

**Files:**
- Modify: `docs/architecture.md`
- Modify: `crates/storage/INTEGRATION-NOTES.md` (and/or `crates/storage/STAGE1-ACCEPTANCE.md`)

Per `CLAUDE.md` → "Keep the docs in sync". (The `TASKS.md` index flip and issue close are handled on `main` after merge by the `/next-task` workflow, not here.)

- [ ] **Step 1: Flip the SPEC-02 snapshot/MVCC Status in `docs/architecture.md`**

Open `docs/architecture.md`, locate the SPEC-02 storage row(s) mentioning snapshot isolation / MVCC / copy-on-write (grep for `snapshot`, `MVCC`, `copy-on-write`, or `SPEC-02`). Update the **Status** for copy-on-write snapshot isolation from `planned`/`specified` to `implemented`, keeping true per-tuple MVCC as `deferred`. Example edit (match the file's actual table/section format):

```
- Copy-on-write snapshot isolation (concurrent-read / single-writer): **implemented** (SPEC-02 #19) — pinned `Store::snapshot()` read transactions; immutable versioned `TierSnapshot`. True per-tuple-visibility MVCC remains **deferred** (Stage 2, intersects SPEC-06).
```

- [ ] **Step 2: Record the delivery in the crate notes**

Append a short subsection to `crates/storage/INTEGRATION-NOTES.md` (or update the `MVCC / copy-on-write snapshots` row in `STAGE1-ACCEPTANCE.md` from "out of scope / #19" to "delivered"):

```markdown
## Copy-on-write snapshot isolation (SPEC-02 #19, delivered)

`MemoryTier` holds an immutable, versioned `Arc<TierSnapshot>` behind
`RwLock<Arc<…>>` plus a writer `Mutex`. `insert_quad_batch` is copy-on-write:
it clones the top-level graph map (Arc clones of untouched graphs), rebuilds
only the affected graphs' partition maps, bumps the version, and atomically
swaps the live pointer. `Store::snapshot()` / `StoreSnapshot` pin a stable,
internally-consistent read view; concurrent writers never disturb a pinned
snapshot, which stays readable until dropped. The dictionary is append-only, so
pinned term ids never change meaning. HDT export reads one pinned snapshot, so a
checkpoint taken under concurrent writes is internally consistent (NF5). True
per-tuple-visibility MVCC remains deferred to Stage 2 (SPEC-06).
```

- [ ] **Step 3: Verify docs reference real symbols**

Run: `grep -n "TierSnapshot\|StoreSnapshot\|snapshot()" crates/storage/src/*.rs | head`
Expected: the symbols named in the docs exist in the source.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md crates/storage/INTEGRATION-NOTES.md crates/storage/STAGE1-ACCEPTANCE.md
git commit -m "docs(storage): record CoW snapshot isolation delivery (SPEC-02 #19)"
```

---

## Task 6: Full-workspace verification gate

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Clippy (what CI enforces)**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings. Watch for: an unused `MemoryTier` import in `store.rs`, a needless `Arc` clone lint, or a `clippy::type_complexity` on the `HashMap<GraphId, HashMap<TermId, PartitionBuilder>>` local (if flagged, add a `// allow`-free refactor or a local type alias — prefer extracting `type GraphBatches = HashMap<GraphId, HashMap<TermId, PartitionBuilder>>;` in `memory_tier.rs`).

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: PASS. Downstream crates (`horndb-wcoj`, `horndb-sparql`, `horndb-harness`) consume `Store`'s unchanged public read methods, so they must still compile and pass.

- [ ] **Step 4: SPARQL server feature build (consumer of storage reads)**

Run: `cargo test -p horndb-sparql --features server 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Commit any fmt-only changes**

```bash
git add -A
git commit -m "chore(storage): fmt/clippy cleanup for CoW snapshots (SPEC-02 #19)" || echo "nothing to commit"
```

---

## Self-Review notes

- **Spec coverage:** Issue #19 scope — "read transaction = stable snapshot id" → `Store::snapshot()` returns `StoreSnapshot` with `version()` (Task 2); "concurrent readers see a consistent snapshot while a single writer appends" → Task 1 CoW + Task 3 concurrency test; "copy-on-write of affected partitions on write; old snapshot stays readable until released" → Task 1 (`Arc`-shared partitions, per-graph CoW) + Task 3 `older_snapshot_outlives_newer_writes`; "NF5 crash-consistency note honoured; no WAL" → Task 4 snapshot-consistent export, no WAL added. Out of scope (per-tuple MVCC) explicitly left deferred in Task 5 docs.
- **Type consistency:** `TierSnapshot::{version,with_predicate,ordered_predicate,predicates,graphs,triple_count,top_predicates,stats}` defined in Task 1 and consumed by `StoreSnapshot`/`MemoryTier` delegators in Tasks 1–2. `StoreSnapshot::{version,triple_count,stats,scan_predicate_default_graph,scan_predicate_ordered,top_predicates,scan_all_term_ids}` defined in Task 2 and used in Tasks 3–4. `build_with_hot_threshold`, `scan`, `ordered`, `len`, `estimated_bytes` are existing `PredicatePartition`/`PartitionBuilder` methods (unchanged).
- **No placeholders:** every code step shows full code; the one runtime-dependent value (final snapshot version literal) has an explicit recompute-and-correct step (Task 3 Steps 2–3) — the correct value is `1001`.
