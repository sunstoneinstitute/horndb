---
status: draft
date: 2026-07-20
scope: "SPEC-25 S1 — per-tuple MVCC visibility stamps + delete path on the copy-on-write memory tier; retire the sparql DELETE DATA tombstone overlay"
---

# Per-tuple MVCC visibility + delete path (SPEC-25 S1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every stored tuple a begin/end lifetime on the tier commit clock, add a batch delete path (`retract_quad_batch`), make every read evaluate visibility at the pinned version, add a compaction pass and the SPEC-24 S6 snapshot surface, and retire the `horndb-sparql` `DELETE DATA` tombstone overlay.

**Architecture:** Keep the existing copy-on-write substrate (immutable `Arc<TierSnapshot>` swapped per commit — this already gives whole-tier snapshot isolation by pointer). Add two parallel stamp columns (`begin`, `end`) to each `PredicatePartition`, one per row, in the same physical order as the `(subject, object)` columns. A tuple is visible at commit version `v` iff `begin ≤ v < end`; `end == UNSET_END` (`u64::MAX`) means live. Insert stamps `begin = commit_version, end = UNSET_END`; retract stamps `end = commit_version` on the matching live row (the tuple stays physically present — a delete is a stamp, not an eviction). Reads apply the `begin ≤ v < end` filter, with a **zero-copy fast path** when a partition has no retracted rows (the insert-only common case, so the WCOJ hot-path benches do not regress). Compaction produces a fresh partition dropping rows whose `end ≤ min_pinned_version`; it never mutates a pinned view (those hold their own older `Arc`s). Once native retraction exists, the sparql `HornEngine` drops its `tombstones: HashSet` overlay and deletes through the store.

**Tech Stack:** Rust 1.90, `arrow` `UInt64Array` columns, `roaring::RoaringTreemap` side-sets, `parking_lot` locks, `criterion` for the micro-bench. Crates touched: `horndb-storage` (core), `horndb-sparql` (overlay retirement), plus doc sync.

**Invariant this plan preserves (state it in every storage commit message):** at any version `v`, at most one row per `(subject, object)` in a partition satisfies `begin ≤ v < end`. This holds because inserts dedup against the currently-live row and a re-insert of a retracted tuple can only begin at a version `≥` the prior row's `end` (you must retract before re-inserting), so a tuple's successive `[begin, end)` ranges are disjoint. The `begin ≤ v < end` read filter therefore yields a duplicate-free set — required so the WCOJ leapfrog trie never sees a repeated key.

**Clock binding (ADR-0018, settled):** the tier commit version *is* the engine's logical clock; `logical_time()` on a snapshot returns it. One tick = one atomic storage batch. This plan does not persist any tick↔version mapping.

---

## File structure

Storage crate (`crates/storage/src/`):

- `visibility.rs` — **new.** `CommitVersion` alias, `UNSET_END` sentinel, the `visible(begin, end, at) -> bool` predicate, and a `filter_visible` helper that takes stamped columns + a version and returns the retained row indices. One small, focused module every other file calls.
- `partition.rs` — **modify.** `PredicatePartition` gains `begin`/`end` `Arc<UInt64Array>` columns (subject-major) and stamps inside `ObjectMajor`. New version-aware accessors (`scan_at`, `ordered_at`, `subject_set_at`, `object_set_at`, `len_at`, `has_retractions`, `retract_matching`). `PartitionBuilder` gains `append_stamped`.
- `ordering.rs` — unchanged (axis mapping still holds).
- `memory_tier.rs` — **modify.** `insert_quad_batch` stamps `begin`; new `retract_quad_batch` stamps `end`; version-parameterized read helpers; a pin registry (`Mutex<BTreeMap<u64, usize>>`) so `compact()` knows the min pinned version; `compact()` itself.
- `tier.rs` — **modify.** `Tier` trait gains `retract_quad_batch`; `TierStats` unchanged.
- `store.rs` — **modify.** `Store::retract_triples`/`retract_quads`; `StoreSnapshot` reads filter at the pinned version; new SPEC-24 S6 surface methods (`contains`, `iter_all_term_ids` ordered, `len`, `logical_time`).
- `snapshot/mod.rs` — **modify.** Export scans the pinned snapshot's *visible* rows only (already routed through `scan_all_term_ids`, so this falls out once that filters).
- `benches/insert_retract.rs` — **new.** criterion micro-bench: insert throughput unchanged + retract-then-read cost (local smoke; the hornbench NF4 write-amp comparison is a filed follow-up, not this PR).

SPARQL crate (`crates/sparql/src/exec/horn.rs`): **modify.** Delete the `tombstones` field and its bookkeeping; `DELETE DATA` and pattern delete call `store.retract_*`; the WCOJ snapshot rebuild reads already-filtered rows.

Docs: `docs/architecture.md` (Status flip), `crates/storage/INTEGRATION-NOTES.md` + `STAGE1-ACCEPTANCE.md` (record the delete path + deferred bench), `docs/plans/PLAN-25-01-*.md` (this file, flip to `executed`).

---

## Task 1: Visibility primitives module

**Files:**
- Create: `crates/storage/src/visibility.rs`
- Modify: `crates/storage/src/lib.rs` (add `pub mod visibility;` and re-exports)
- Test: inline `#[cfg(test)]` in `visibility.rs`

- [ ] **Step 1: Write the failing test**

In `crates/storage/src/visibility.rs`:

```rust
//! Per-tuple MVCC visibility primitives (SPEC-25 S1).
//!
//! Every stored tuple carries a `[begin, end)` lifetime in tier commit-version
//! terms (ADR-0018: the commit version is the engine's logical clock). A tuple
//! is visible at version `v` iff `begin <= v < end`. `end == UNSET_END` means
//! the tuple is still live (never retracted).

/// A tier commit version. Monotonic, bumped once per committed batch. `0` is
/// the empty store; the first commit is version `1`.
pub type CommitVersion = u64;

/// Sentinel `end` stamp for a live (never-retracted) tuple. No real commit
/// version reaches `u64::MAX`, so `v < UNSET_END` is always true for a live row.
pub const UNSET_END: CommitVersion = u64::MAX;

/// True if a tuple stamped `[begin, end)` is visible at version `at`.
#[inline]
pub fn visible(begin: CommitVersion, end: CommitVersion, at: CommitVersion) -> bool {
    begin <= at && at < end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_tuple_visible_from_its_begin_onward() {
        // Inserted at v=5, never retracted.
        assert!(!visible(5, UNSET_END, 4), "not yet inserted");
        assert!(visible(5, UNSET_END, 5), "visible at its own insert version");
        assert!(visible(5, UNSET_END, 999), "still visible far later");
    }

    #[test]
    fn retraction_takes_effect_at_its_own_version() {
        // Inserted at v=5, retracted at v=8.
        assert!(visible(5, 8, 7), "visible just before retraction");
        assert!(!visible(5, 8, 8), "hidden at the retraction version");
        assert!(!visible(5, 8, 9), "still hidden after");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-storage visibility::`
Expected: FAIL — `visibility` module not declared in `lib.rs`.

- [ ] **Step 3: Wire the module into `lib.rs`**

In `crates/storage/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod visibility;
```

and re-export the primitives near the other re-exports:

```rust
pub use visibility::{visible, CommitVersion, UNSET_END};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-storage visibility::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/visibility.rs crates/storage/src/lib.rs
git commit -m "storage(mvcc): add per-tuple visibility primitives (SPEC-25 S1)"
```

---

## Task 2: Stamp columns on `PredicatePartition` (subject-major)

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Test: inline `#[cfg(test)]` in `partition.rs`

Add `begin`/`end` `Arc<UInt64Array>` columns parallel to `subjects`/`objects`, extend the builder to carry stamps, and add version-aware read accessors with a zero-copy fast path. This task covers only the subject-major layout and `scan`; object-major and sets follow in Tasks 3–4.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/storage/src/partition.rs`:

```rust
#[test]
fn stamped_scan_filters_by_version() {
    use crate::visibility::UNSET_END;
    let mut b = PartitionBuilder::default();
    // (1,10) inserted at v1, live; (2,20) inserted at v1 then retracted at v3.
    b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
    b.append_stamped(TermId(2), TermId(20), 1, 3);
    let part = b.build();

    // At v2: both visible (retraction not yet in effect).
    let at2: Vec<_> = part.scan_at(2).collect();
    assert_eq!(at2, vec![(TermId(1), TermId(10)), (TermId(2), TermId(20))]);

    // At v3: (2,20) hidden (v3 == end).
    let at3: Vec<_> = part.scan_at(3).collect();
    assert_eq!(at3, vec![(TermId(1), TermId(10))]);

    assert_eq!(part.len_at(2), 2);
    assert_eq!(part.len_at(3), 1);
}

#[test]
fn has_retractions_reports_dead_rows() {
    use crate::visibility::UNSET_END;
    let mut live = PartitionBuilder::default();
    live.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
    assert!(!live.build().has_retractions(), "no dead rows");

    let mut dead = PartitionBuilder::default();
    dead.append_stamped(TermId(1), TermId(10), 1, 2);
    assert!(dead.build().has_retractions(), "one dead row");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage partition::tests::stamped_scan_filters_by_version`
Expected: FAIL — `append_stamped`/`scan_at`/`len_at`/`has_retractions` do not exist.

- [ ] **Step 3: Add the stamp columns, builder, and accessors**

In `crates/storage/src/partition.rs`:

Add imports at the top:

```rust
use crate::visibility::{visible, CommitVersion, UNSET_END};
```

Add fields to `PredicatePartition` (after `objects`):

```rust
    // Per-row visibility stamps, aligned 1:1 with the subject-major columns.
    // `end[i] == UNSET_END` means row i is live. Object-major carries its own
    // re-sorted copies (see `ObjectMajor`).
    begin: Arc<UInt64Array>,
    end: Arc<UInt64Array>,
    // True once any row has a set `end` (a retraction). Lets read paths take a
    // zero-copy fast path when the partition is insert-only.
    has_retractions: bool,
```

Add accessors on `impl PredicatePartition` (near `scan`):

```rust
    /// True if any row in this partition has been retracted (`end` set). When
    /// false, every version-aware read returns the raw columns with no filter.
    pub fn has_retractions(&self) -> bool {
        self.has_retractions
    }

    /// The `begin`/`end` stamp columns (subject-major order), for the WAL and
    /// compaction. Aligned 1:1 with `subjects()`/`objects()`.
    pub fn begins(&self) -> &UInt64Array {
        &self.begin
    }
    pub fn ends(&self) -> &UInt64Array {
        &self.end
    }

    /// Scan `(subject, object)` rows visible at `at`, in subject-major order.
    /// Zero-filter fast path when the partition is insert-only.
    pub fn scan_at(&self, at: CommitVersion) -> impl Iterator<Item = (TermId, TermId)> + '_ {
        let filtered = self.has_retractions;
        (0..self.len()).filter_map(move |i| {
            if filtered && !visible(self.begin.value(i), self.end.value(i), at) {
                None
            } else {
                Some((TermId(self.subjects.value(i)), TermId(self.objects.value(i))))
            }
        })
    }

    /// Count of rows visible at `at`.
    pub fn len_at(&self, at: CommitVersion) -> usize {
        if !self.has_retractions {
            return self.len();
        }
        (0..self.len())
            .filter(|&i| visible(self.begin.value(i), self.end.value(i), at))
            .count()
    }
```

Update `PartitionBuilder` to carry stamps. Change the field and methods:

```rust
#[derive(Default)]
pub struct PartitionBuilder {
    // (subject, object, begin, end) rows.
    rows: Vec<(u64, u64, CommitVersion, CommitVersion)>,
}

impl PartitionBuilder {
    /// Append a live row (used by legacy/test call sites that predate stamps):
    /// begin 0, end UNSET_END — visible at every version.
    pub fn append(&mut self, s: TermId, o: TermId) {
        self.rows.push((s.0, o.0, 0, UNSET_END));
    }

    /// Append a row with explicit visibility stamps.
    pub fn append_stamped(
        &mut self,
        s: TermId,
        o: TermId,
        begin: CommitVersion,
        end: CommitVersion,
    ) {
        self.rows.push((s.0, o.0, begin, end));
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}
```

Rewrite `build_with_hot_threshold` to sort by `(s, o, begin)`, dedup **only exact-duplicate live rows** (identical `(s, o)` with `end == UNSET_END`), keep dead rows, and populate stamp columns:

```rust
    pub fn build_with_hot_threshold(mut self, hot_threshold: usize) -> PredicatePartition {
        // Sort by (subject, object, begin) so the (s, o) columns stay in SPO
        // order for trie iteration; begin orders a tuple's history.
        self.rows.sort_unstable_by(|a, b| {
            a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
        });
        // Collapse only exact-duplicate *live* rows for the same (s, o): a
        // repeated insert is a no-op. Dead rows (end set) are history and are
        // kept until compaction.
        self.rows.dedup_by(|a, b| {
            a.0 == b.0 && a.1 == b.1 && a.3 == UNSET_END && b.3 == UNSET_END
        });

        let n = self.rows.len();
        let mut subj_set = RoaringTreemap::new();
        let mut obj_set = RoaringTreemap::new();
        let mut s_col = Vec::with_capacity(n);
        let mut o_col = Vec::with_capacity(n);
        let mut begin_col = Vec::with_capacity(n);
        let mut end_col = Vec::with_capacity(n);
        let mut has_retractions = false;
        for (s, o, begin, end) in &self.rows {
            s_col.push(*s);
            o_col.push(*o);
            begin_col.push(*begin);
            end_col.push(*end);
            if *end != UNSET_END {
                has_retractions = true;
            }
            // Side-sets are supersets across all versions; version-exact sets
            // are computed on demand (Task 4).
            subj_set.insert(TermId(*s).payload());
            obj_set.insert(TermId(*o).payload());
        }
        let partition = PredicatePartition {
            subjects: Arc::new(UInt64Array::from(s_col)),
            objects: Arc::new(UInt64Array::from(o_col)),
            begin: Arc::new(UInt64Array::from(begin_col)),
            end: Arc::new(UInt64Array::from(end_col)),
            has_retractions,
            subject_set: subj_set,
            object_set: obj_set,
            object_major: OnceLock::new(),
        };
        if partition.len_at(u64::MAX - 1) >= hot_threshold {
            let _ = partition.object_major.set(partition.build_object_major());
        }
        partition
    }
```

> Note on the hot-threshold check: use `len_at(u64::MAX - 1)` (the live-row count) rather than `len()` so a partition full of dead history is not falsely "hot". `u64::MAX - 1` is a safe "latest" probe version since real commit versions never approach it.

The `estimated_bytes` calc should account for the two extra 8-byte stamp columns. Update it:

```rust
    pub fn estimated_bytes(&self) -> u64 {
        let rows = self.len() as u64;
        // 16 B for (s, o) + 16 B for (begin, end) stamps.
        let base = rows * 32;
        if self.object_major_materialized() {
            base + rows * 16
        } else {
            base
        }
    }
```

- [ ] **Step 4: Run the new tests + existing partition tests**

Run: `cargo test -p horndb-storage partition::`
Expected: PASS — new tests plus the existing `scan_objects_equal_*` tests (they use `append`, now stamped live).

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/partition.rs
git commit -m "storage(mvcc): begin/end stamp columns + version-aware scan on partitions (SPEC-25 S1)"
```

---

## Task 3: Version-aware object-major layout + `ordered_at`

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Test: inline `#[cfg(test)]` in `partition.rs`

The object-major layout must carry stamps too, and `ordered()` must become version-aware so WCOJ trie iteration over any ordering sees only visible rows (no duplicate keys).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn ordered_at_filters_both_axes() {
    use crate::ordering::Ordering;
    use crate::visibility::UNSET_END;
    let mut b = PartitionBuilder::default();
    b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
    b.append_stamped(TermId(2), TermId(20), 1, 3); // retracted at v3
    let part = b.build();

    // Object-major (Pos) at v3 must also drop the retracted row.
    let cols = part.ordered_at(Ordering::Pos, 3);
    let rows: Vec<_> = cols.subject_object().collect();
    assert_eq!(rows, vec![(TermId(1), TermId(10))]);

    // At v2 both rows present, object-major sorted by (object, subject).
    let cols2 = part.ordered_at(Ordering::Pos, 2);
    let rows2: Vec<_> = cols2.subject_object().collect();
    assert_eq!(rows2, vec![(TermId(1), TermId(10)), (TermId(2), TermId(20))]);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage partition::tests::ordered_at_filters_both_axes`
Expected: FAIL — `ordered_at` does not exist.

- [ ] **Step 3: Carry stamps through `ObjectMajor`, add `ordered_at`**

Extend `ObjectMajor`:

```rust
struct ObjectMajor {
    objects: Arc<UInt64Array>,
    subjects: Arc<UInt64Array>,
    begin: Arc<UInt64Array>,
    end: Arc<UInt64Array>,
}
```

Update `build_object_major` to re-sort stamps alongside the columns:

```rust
    fn build_object_major(&self) -> ObjectMajor {
        let n = self.len();
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_unstable_by(|&a, &b| {
            self.objects
                .value(a)
                .cmp(&self.objects.value(b))
                .then_with(|| self.subjects.value(a).cmp(&self.subjects.value(b)))
        });
        let mut o_col = Vec::with_capacity(n);
        let mut s_col = Vec::with_capacity(n);
        let mut b_col = Vec::with_capacity(n);
        let mut e_col = Vec::with_capacity(n);
        for &i in &idx {
            o_col.push(self.objects.value(i));
            s_col.push(self.subjects.value(i));
            b_col.push(self.begin.value(i));
            e_col.push(self.end.value(i));
        }
        ObjectMajor {
            objects: Arc::new(UInt64Array::from(o_col)),
            subjects: Arc::new(UInt64Array::from(s_col)),
            begin: Arc::new(UInt64Array::from(b_col)),
            end: Arc::new(UInt64Array::from(e_col)),
        }
    }
```

Add `ordered_at`. Keep the old `ordered` as a "latest live" convenience that forwards with a sentinel version, so existing call sites keep compiling until they migrate (Tasks 5–7):

```rust
    /// Ordered access to rows visible at `at`, in any of the six orderings.
    /// Zero-copy when the partition is insert-only (raw columns shared by
    /// `Arc`); otherwise the visible subset is materialized once for this call.
    pub fn ordered_at(&self, ord: Ordering, at: CommitVersion) -> OrderedColumns {
        let (level0, level1, begin, end, axis) = match ord.axis() {
            PartitionAxis::SubjectMajor => (
                self.subjects.clone(),
                self.objects.clone(),
                self.begin.clone(),
                self.end.clone(),
                PartitionAxis::SubjectMajor,
            ),
            PartitionAxis::ObjectMajor => {
                let om = self.object_major.get_or_init(|| self.build_object_major());
                (
                    om.objects.clone(),
                    om.subjects.clone(),
                    om.begin.clone(),
                    om.end.clone(),
                    PartitionAxis::ObjectMajor,
                )
            }
        };
        if !self.has_retractions {
            return OrderedColumns { axis, level0, level1 };
        }
        // Materialize the visible subset, preserving sort order.
        let n = level0.len();
        let mut l0 = Vec::with_capacity(n);
        let mut l1 = Vec::with_capacity(n);
        for i in 0..n {
            if visible(begin.value(i), end.value(i), at) {
                l0.push(level0.value(i));
                l1.push(level1.value(i));
            }
        }
        OrderedColumns {
            axis,
            level0: Arc::new(UInt64Array::from(l0)),
            level1: Arc::new(UInt64Array::from(l1)),
        }
    }
```

Keep the existing `ordered(&self, ord)` method but have it delegate:

```rust
    /// Latest-live ordered access (all rows not yet retracted). Convenience for
    /// call sites that always read the newest committed state.
    pub fn ordered(&self, ord: Ordering) -> OrderedColumns {
        self.ordered_at(ord, u64::MAX - 1)
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p horndb-storage partition::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/partition.rs
git commit -m "storage(mvcc): version-aware ordered() over both axes (SPEC-25 S1)"
```

---

## Task 4: Version-exact Roaring subject/object sets

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Test: inline `#[cfg(test)]` in `partition.rs`

`subject_set()`/`object_set()` today are supersets across all versions. Add `subject_set_at`/`object_set_at` that are exact at a version (dropping payloads with no visible row). Fast path returns a borrow of the prebuilt set when insert-only.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn object_set_at_drops_retracted_only_payloads() {
    use crate::visibility::UNSET_END;
    let mut b = PartitionBuilder::default();
    b.append_stamped(TermId(1), TermId(10), 1, UNSET_END);
    b.append_stamped(TermId(2), TermId(20), 1, 3); // object 20 only via a retracted row
    let part = b.build();

    // At v2 both objects present.
    assert!(part.object_set_at(2).contains(TermId(20).payload()));
    // At v3 object 20 has no visible row → absent from the exact set.
    assert!(!part.object_set_at(3).contains(TermId(20).payload()));
    assert!(part.object_set_at(3).contains(TermId(10).payload()));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage partition::tests::object_set_at_drops_retracted_only_payloads`
Expected: FAIL — `object_set_at` does not exist.

- [ ] **Step 3: Add the version-exact set accessors**

Use `std::borrow::Cow` so the insert-only fast path avoids rebuilding:

```rust
    /// Distinct subject payloads with at least one row visible at `at`.
    /// Borrows the prebuilt superset when the partition is insert-only.
    pub fn subject_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        if !self.has_retractions {
            return std::borrow::Cow::Borrowed(&self.subject_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..self.len() {
            if visible(self.begin.value(i), self.end.value(i), at) {
                set.insert(TermId(self.subjects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }

    /// Distinct object payloads with at least one row visible at `at`.
    pub fn object_set_at(&self, at: CommitVersion) -> std::borrow::Cow<'_, RoaringTreemap> {
        if !self.has_retractions {
            return std::borrow::Cow::Borrowed(&self.object_set);
        }
        let mut set = RoaringTreemap::new();
        for i in 0..self.len() {
            if visible(self.begin.value(i), self.end.value(i), at) {
                set.insert(TermId(self.objects.value(i)).payload());
            }
        }
        std::borrow::Cow::Owned(set)
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p horndb-storage partition::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/partition.rs
git commit -m "storage(mvcc): version-exact Roaring subject/object sets (SPEC-25 S1)"
```

---

## Task 5: `retract_quad_batch` on the tier + `insert` stamps begin

**Files:**
- Modify: `crates/storage/src/tier.rs`, `crates/storage/src/memory_tier.rs`
- Test: inline `#[cfg(test)]` in `memory_tier.rs`

Insert stamps `begin = new_version`. Retraction rebuilds affected partitions stamping `end = new_version` on the single live row matching each `(g, s, p, o)`; absent/already-dead rows are counted no-ops. Both remain one-batch-one-version.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/storage/src/memory_tier.rs`:

```rust
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

    assert_eq!(before.triple_count(), 1, "snapshot pinned before delete still sees it");
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
}

#[test]
fn reinsert_after_retract_is_live_again() {
    let tier = MemoryTier::new();
    let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
    tier.insert_quad_batch(&[q]).unwrap();
    tier.retract_quad_batch(&[q]).unwrap();
    tier.insert_quad_batch(&[q]).unwrap();
    assert_eq!(tier.snapshot().triple_count(), 1, "tuple live after re-insert");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage memory_tier::tests::retract_hides_from_later_snapshot_only`
Expected: FAIL — `retract_quad_batch` does not exist; `triple_count` on a snapshot is not version-filtered yet.

- [ ] **Step 3: Add the trait method**

In `crates/storage/src/tier.rs`, add to `trait Tier` (after `insert_quad_batch`):

```rust
    /// Retract a batch of quads. Stamps each matching live tuple's `end` at the
    /// new commit version (one batch = one version). Retracting an absent or
    /// already-dead tuple is a counted no-op, not an error. Returns the number
    /// of tuples actually retracted.
    fn retract_quad_batch(
        &self,
        quads: &[(GraphId, TermId, TermId, TermId)],
    ) -> Result<usize>;
```

- [ ] **Step 4: Implement stamping in `memory_tier.rs`**

First, make `insert_quad_batch` stamp `begin` at the new version and carry existing rows' stamps. Replace the partition-rebuild loop body so it uses `append_stamped`:

```rust
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        if quads.is_empty() {
            return Ok(());
        }
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = cur.version + 1;

        // Group incoming pairs by graph, then predicate.
        let mut by_graph: HashMap<GraphId, HashMap<TermId, Vec<(TermId, TermId)>>> =
            HashMap::new();
        for &(g, s, p, o) in quads {
            by_graph.entry(g).or_default().entry(p).or_default().push((s, o));
        }

        let mut graphs = cur.graphs.clone();
        for (g, pred_rows) in by_graph {
            let mut new_partitions = graphs
                .get(&g)
                .map(|gs| gs.partitions.clone())
                .unwrap_or_default();
            for (p, rows) in pred_rows {
                let mut builder = PartitionBuilder::default();
                // Carry existing rows with their stamps (history preserved).
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
            graphs.insert(g, Arc::new(GraphStore { partitions: new_partitions }));
        }

        let next = Arc::new(TierSnapshot { version: new_version, graphs });
        *self.current.write() = next;
        Ok(())
    }
```

Add imports at the top of `memory_tier.rs`:

```rust
use crate::term::TermId;
use crate::visibility::UNSET_END;
```

(Confirm `TermId` is imported; it already is via the existing `use crate::term::{GraphId, TermId}`.)

Implement `retract_quad_batch` (add to the `impl Tier for MemoryTier` block):

```rust
    fn retract_quad_batch(
        &self,
        quads: &[(GraphId, TermId, TermId, TermId)],
    ) -> Result<usize> {
        if quads.is_empty() {
            return Ok(0);
        }
        let _w = self.writer.lock();
        let cur = self.current.read().clone();
        let new_version = cur.version + 1;

        // Group targets by graph, then predicate, as a set of (s, o) to end.
        let mut by_graph: HashMap<GraphId, HashMap<TermId, HashSet<(u64, u64)>>> =
            HashMap::new();
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
            let Some(gs) = graphs.get(&g) else { continue };
            let mut new_partitions = gs.partitions.clone();
            for (p, targets) in pred_targets {
                let Some(existing) = new_partitions.get(&p) else { continue };
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
            graphs.insert(g, Arc::new(GraphStore { partitions: new_partitions }));
        }

        // Only bump the clock / swap if something changed, so a fully-absent
        // retraction batch is a true no-op (no dead version created).
        if retracted > 0 {
            let next = Arc::new(TierSnapshot { version: new_version, graphs });
            *self.current.write() = next;
        }
        Ok(retracted)
    }
```

Add `use std::collections::HashSet;` to the imports.

> **Design note for the reviewer:** a target `(s, o)` may match at most one live row (the plan's invariant), so `retracted` counts tuples, not rows scanned. A batch that retracts nothing does not bump the version — this keeps idempotent replay (S3) from minting empty versions, and matches "counted no-op".

- [ ] **Step 5: Make `TierSnapshot` reads version-filtered**

`TierSnapshot::triple_count`, `stats`, `top_predicates`, and `with_predicate` must evaluate at `self.version`. Update `triple_count`:

```rust
    pub fn triple_count(&self) -> u64 {
        self.graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len_at(self.version) as u64)
            .sum()
    }
```

Update `top_predicates` and `stats` to use `len_at(self.version)` in place of `len()`. Add a version accessor used by later tasks (already present: `version()`).

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p horndb-storage memory_tier::`
Expected: PASS — the three new tests plus the existing snapshot/insert tests (existing tests insert only, so `len_at == len`).

- [ ] **Step 7: Commit**

```bash
git add crates/storage/src/tier.rs crates/storage/src/memory_tier.rs
git commit -m "storage(mvcc): retract_quad_batch + begin-stamped inserts + version-filtered counts (SPEC-25 S1)"
```

---

## Task 6: Pin registry + compaction

**Files:**
- Modify: `crates/storage/src/memory_tier.rs`, `crates/storage/src/store.rs`
- Test: inline `#[cfg(test)]` in `memory_tier.rs`

Track outstanding pinned versions so `compact()` can reclaim rows whose `end ≤ min_pinned` without changing any pinned view. Pins are held by a guard that decrements on drop.

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage memory_tier::tests::compaction_reclaims_only_below_min_pin`
Expected: FAIL — `compact` does not exist; `snapshot()` does not register a pin.

- [ ] **Step 3: Add the pin registry to `MemoryTier`**

Add a field and a guard type. The guard holds an `Arc` to the registry so it can decrement on drop independently of the tier's lifetime borrow:

```rust
use std::collections::BTreeMap;

pub struct MemoryTier {
    current: RwLock<Arc<TierSnapshot>>,
    writer: Mutex<()>,
    hot_threshold: usize,
    // version -> number of live pins at that version. Empty ⇒ no pins.
    pins: Arc<Mutex<BTreeMap<u64, usize>>>,
}
```

Add a pinned-snapshot guard that wraps the `Arc<TierSnapshot>`:

```rust
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
```

Keep `snapshot()` returning `Arc<TierSnapshot>` for existing call sites, but add a pinning variant and route compaction through the registry. To avoid churn, change `snapshot()` to register a pin and return a `PinnedSnapshot`, and update the internal callers that need a bare `Arc` to call `.arc()`:

```rust
    /// Pin the current immutable tier state and register the pin so compaction
    /// will not reclaim rows still visible to it. The pin is released when the
    /// returned guard drops.
    pub fn snapshot(&self) -> PinnedSnapshot {
        let snap = self.current.read().clone();
        *self.pins.lock().entry(snap.version).or_insert(0) += 1;
        PinnedSnapshot { snap, pins: self.pins.clone() }
    }

    /// Lowest pinned version, or the current version if nothing is pinned.
    fn min_pinned(&self) -> u64 {
        let pins = self.pins.lock();
        pins.keys().next().copied().unwrap_or_else(|| self.current.read().version)
    }

    /// Reclaim dead rows whose `end <= min_pinned`. Rebuilds only partitions
    /// that actually hold reclaimable rows; never changes a pinned view (those
    /// hold their own older `Arc`s). Does not bump the version.
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
                let reclaimable = (0..part.len())
                    .any(|i| part.ends().value(i) <= horizon);
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
                graphs.insert(*g, Arc::new(GraphStore { partitions: new_partitions }));
                changed = true;
            }
        }
        if changed {
            // Same version: compaction is not a logical write.
            let next = Arc::new(TierSnapshot { version: cur.version, graphs });
            *self.current.write() = next;
        }
    }
```

Update `MemoryTier::with_hot_threshold` and `new` to initialise `pins: Arc::new(Mutex::new(BTreeMap::new()))`.

Update the internal `MemoryTier` helper methods (`predicates`, `graphs`, `triple_count`, `stats`, `with_predicate`, `ordered_predicate`, `top_predicates`) that currently call `self.snapshot().X` — with `snapshot()` now returning a guard, `self.snapshot().predicates(g)` still works via `Deref`, but the guard is created and dropped per call (registers/unregisters a transient pin). That is correct but adds lock traffic. Leave them as-is for correctness; they Deref through the guard.

- [ ] **Step 4: Fix `store.rs` snapshot plumbing**

`Store::snapshot()` calls `mt.snapshot()` and stores the resulting `Arc<TierSnapshot>` in `StoreSnapshot.tier`. Change `StoreSnapshot` to hold the `PinnedSnapshot` guard so the pin lives as long as the read transaction:

```rust
pub struct StoreSnapshot<'a> {
    tier: crate::memory_tier::PinnedSnapshot,
    dictionary: &'a Dictionary,
}
```

and in `Store::snapshot`:

```rust
        StoreSnapshot {
            tier: mt.snapshot(),
            dictionary: &self.dictionary,
        }
```

All `self.tier.X` calls in `StoreSnapshot` continue to work through `Deref`. Export the guard type from `memory_tier` (`pub struct PinnedSnapshot`) and re-export if `TierSnapshot` is re-exported in `lib.rs`.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p horndb-storage`
Expected: PASS — compaction tests plus all prior storage tests.

- [ ] **Step 6: Commit**

```bash
git add crates/storage/src/memory_tier.rs crates/storage/src/store.rs
git commit -m "storage(mvcc): pin registry + snapshot-respecting compaction (SPEC-25 S1)"
```

---

## Task 7: `Store` retract API, version-filtered reads, SPEC-24 S6 surface

**Files:**
- Modify: `crates/storage/src/store.rs`
- Test: inline `#[cfg(test)]` in `store.rs`

Public retraction on `Store`, all `StoreSnapshot` reads filtered at the pinned version, and the SPEC-24 S6 surface: `contains`, ordered `iter`, `len`/`is_empty`, `logical_time`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn store_retract_is_visible_to_new_reads_only() {
    let store = Store::in_memory();
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(&[t.clone()]).unwrap();
    let before = store.snapshot();
    let n = store.retract_triples(&[t.clone()]).unwrap();
    assert_eq!(n, 1);

    assert_eq!(before.triple_count(), 1, "pinned-before read still sees it");
    assert_eq!(store.snapshot().triple_count(), 0, "new read does not");
}

#[test]
fn snapshot_s6_surface() {
    let store = Store::in_memory();
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(&[t.clone()]).unwrap();
    let snap = store.snapshot();

    let (s, p, o) = {
        let d = store.dictionary();
        (
            d.get(&t.0).unwrap(),
            d.get(&t.1).unwrap(),
            d.get(&t.2).unwrap(),
        )
    };
    assert!(snap.contains(s, p, o), "contains a present triple");
    assert!(!snap.contains(s, p, o.wrapping_swap()), "does not contain an absent one");
    assert_eq!(snap.len(), 1);
    assert!(!snap.is_empty());
    assert_eq!(snap.logical_time(), snap.version());

    // Ordered iteration is key-sorted and stable.
    let ids: Vec<_> = snap.iter_all_term_ids().collect();
    assert_eq!(ids.len(), 1);
}
```

> `wrapping_swap` above is shorthand for "some different TermId". Replace it in the real test with a concrete distinct id, e.g. `TermId(o.0 + 1)`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-storage store::tests::store_retract_is_visible_to_new_reads_only`
Expected: FAIL — `retract_triples`, `contains`, `iter_all_term_ids`, `len`, `logical_time` do not exist.

- [ ] **Step 3: Add `Store` retraction**

In `impl Store`:

```rust
    /// Retract triples from the default graph. Returns the number of tuples
    /// actually retracted (absent tuples are counted no-ops). Terms are looked
    /// up, not interned: a triple mentioning an unknown term retracts nothing.
    pub fn retract_triples(&self, triples: &[(Term, Term, Term)]) -> Result<usize> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue; // an un-interned term cannot be stored, so nothing to retract
            };
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.tier.retract_quad_batch(&quads)
    }

    /// Retract quads. `GraphId`s must already have been interned.
    pub fn retract_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        let mut encoded = Vec::with_capacity(quads.len());
        for (g, s, p, o) in quads {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue;
            };
            encoded.push((*g, s_id, p_id, o_id));
        }
        self.tier.retract_quad_batch(&encoded)
    }
```

- [ ] **Step 4: Make `StoreSnapshot` reads version-filtered + add the S6 surface**

`scan_predicate_default_graph`, `scan_predicate_ordered`, `scan_all_term_ids`, and `has_named_graph_data` must read at `self.tier.version()`. Update the partition calls to the `_at` accessors:

- In `scan_predicate_default_graph`, replace `part.scan()` with `part.scan_at(self.tier.version())`.
- In `scan_predicate_ordered`, replace `self.tier.ordered_predicate(...)` with an at-version ordered call (add `MemoryTier::ordered_predicate_at` mirroring `ordered_predicate` but forwarding `at`, and a `TierSnapshot::ordered_predicate_at` that calls `part.ordered_at(ord, self.version)`).
- In `scan_all_term_ids`, replace `part.scan()` with `part.scan_at(self.tier.version())` and size the `Vec` with `self.tier.triple_count()` (already version-filtered from Task 5).
- In `has_named_graph_data`, a named graph counts only if it has a *visible* triple: change the predicate check to `self.tier.predicates(g).iter().any(|&p| self.tier.with_predicate(g, p, |part| part.len_at(self.tier.version()) > 0).unwrap_or(false))`.

Add the S6 surface to `impl StoreSnapshot`:

```rust
    /// SPEC-24 S6 as-of token: the commit version (== logical clock, ADR-0018).
    pub fn logical_time(&self) -> u64 {
        self.tier.version()
    }

    /// Number of triples visible in this pinned view (default graph + named).
    pub fn len(&self) -> usize {
        self.tier.triple_count() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if `(s, p, o)` is visible in the default graph at this version.
    pub fn contains(&self, s: TermId, p: TermId, o: TermId) -> bool {
        self.tier
            .with_predicate(DEFAULT_GRAPH, p, |part| {
                part.scan_at(self.tier.version()).any(|(rs, ro)| rs == s && ro == o)
            })
            .unwrap_or(false)
    }

    /// Key-ordered iteration over every visible default-graph triple as raw
    /// `TermId`s (predicate-partitioned, subject-major within each predicate).
    /// Stable across concurrent writes (pinned view).
    pub fn iter_all_term_ids(&self) -> impl Iterator<Item = (TermId, TermId, TermId)> + '_ {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(DEFAULT_GRAPH);
        preds.sort_by_key(|t| t.0);
        preds.into_iter().flat_map(move |p_id| {
            self.tier
                .with_predicate(DEFAULT_GRAPH, p_id, |part| {
                    part.scan_at(version).map(move |(s, o)| (s, p_id, o)).collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    }
```

> `contains` is O(partition) here — fine for S1 (SPEC-24 S6 point reads run against modest per-predicate partitions; a sorted-column binary search is a later optimization filed with the WCOJ columnar source #239). Note this in a code comment.

Add `use crate::term::TermId;` if not already imported (it is).

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p horndb-storage store::`
Expected: PASS. Replace `o.wrapping_swap()` in the test with `TermId(o.0 + 1)` before running.

- [ ] **Step 6: Full storage suite + clippy**

Run: `cargo test -p horndb-storage` and `cargo clippy -p horndb-storage --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/storage/src/store.rs crates/storage/src/memory_tier.rs
git commit -m "storage(mvcc): Store retraction + version-filtered reads + SPEC-24 S6 surface (SPEC-25 S1)"
```

---

## Task 8: Concurrent reader/writer + snapshot-export retraction tests

**Files:**
- Create: `crates/storage/tests/mvcc_delete.rs`
- (Snapshot export already routes through `scan_all_term_ids`, which now filters — this task proves it.)

Acceptance #1 demands the retraction be honest under concurrency and across snapshot export. These are integration tests against the public `Store` API.

- [ ] **Step 1: Write the failing integration tests**

Create `crates/storage/tests/mvcc_delete.rs`:

```rust
//! SPEC-25 S1 acceptance #1: deletes exist and snapshots stay honest.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::sync::Arc;
use std::thread;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn snapshot_export_excludes_retracted_triples() {
    let store = Store::in_memory();
    let a = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let c = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
    store.insert_triples(&[a.clone(), c.clone()]).unwrap();
    store.retract_triples(&[a.clone()]).unwrap();

    let mut buf = Vec::new();
    let stats = store.export_snapshot(&mut buf).unwrap();
    assert_eq!(stats.triples, 1, "export sees only the live triple");

    // Round-trip into a fresh store: exactly the live triple.
    let restored = Store::in_memory();
    restored.import_snapshot(&mut buf.as_slice()).unwrap();
    assert_eq!(restored.triple_count(), 1);
}

#[test]
fn concurrent_reader_pinned_before_delete_is_stable() {
    let store = Arc::new(Store::in_memory());
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(&[t.clone()]).unwrap();

    let pinned = store.snapshot(); // sees the triple
    let writer = {
        let store = store.clone();
        let t = t.clone();
        thread::spawn(move || {
            for _ in 0..100 {
                store.retract_triples(&[t.clone()]).unwrap();
                store.insert_triples(&[t.clone()]).unwrap();
            }
        })
    };
    // While the writer churns, the pinned view never changes.
    for _ in 0..1000 {
        assert_eq!(pinned.triple_count(), 1);
    }
    writer.join().unwrap();
}
```

- [ ] **Step 2: Run to verify they fail (or pass)**

Run: `cargo test -p horndb-storage --test mvcc_delete`
Expected: PASS if Tasks 1–7 are correct. If `snapshot_export_excludes_retracted_triples` fails, `export_snapshot` is not reading through the filtered `scan_all_term_ids` — trace it and fix in `snapshot/mod.rs`.

- [ ] **Step 3: Commit**

```bash
git add crates/storage/tests/mvcc_delete.rs
git commit -m "storage(mvcc): concurrency + export retraction acceptance tests (SPEC-25 S1)"
```

---

## Task 9: Retire the sparql `DELETE DATA` tombstone overlay

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs`
- Test: existing `crates/sparql/src/exec/horn.rs` tests + `cargo test -p horndb-sparql --features server`

`HornEngine` keeps a `tombstones: HashSet<(u64,u64,u64)>` because storage was insertion-only. Replace overlay deletes with native `store.retract_*`; the WCOJ snapshot rebuild then reads already-filtered rows.

- [ ] **Step 1: Read the current overlay end-to-end**

Read `crates/sparql/src/exec/horn.rs` around lines 200–620 (the `tombstones` field, `insert_oxrdf`, the bulk insert path, `delete`/`clear` paths, `wcoj_snapshot`, and `scan_all_term_ids` read-back). Note every `self.tombstones` touch and the `stored_keys`/`live` count bookkeeping that exists only to net out tombstones.

- [ ] **Step 2: Delete the overlay, delete through storage**

- Remove the `tombstones` field and its initialisation.
- The single-triple delete path (around line 596): replace
  `if !self.tombstones.contains(&key) && self.is_in_storage(key) { self.tombstones.insert(key); }`
  with a call that retracts the triple from the store (build the three `Term`s or reuse the already-encoded ids via a new `Store::retract_quads`/`retract_triples`) and invalidates the WCOJ snapshot.
- The clear/delete-all path (around line 606, `self.tombstones = self.stored_keys.clone();`) becomes a store retraction of every currently-live default-graph triple (iterate `store.snapshot().iter_all_term_ids()` into `retract` by ids — add a `Store::retract_quad_ids(&[(GraphId, TermId, TermId, TermId)])` thin wrapper over `tier.retract_quad_batch` to avoid a term round-trip), then invalidate.
- `wcoj_snapshot` (line 469) and the `scan_all_term_ids` read-back (line 461): drop the `.filter(|k| !self.tombstones.contains(k))` — `store.scan_all_term_ids()` is now visibility-filtered. The live-count bookkeeping (`live`) collapses to `store.triple_count()`.
- Re-insert-after-delete "resurrection" (tests at lines 1257, 1383): with native retraction, re-inserting a retracted triple stamps a fresh live row (Task 5 `reinsert_after_retract_is_live_again`), so the existing behavior holds without tombstone-clearing logic. Keep the tests; adjust only if they assert on the removed internal fields.

> If reusing raw ids is awkward, the simplest correct path is: `DELETE DATA` maps its quads to `Term`s (it already has them from the parse) and calls `store.retract_triples`/`retract_quads`. Only the whole-graph clear benefits from the id-level wrapper.

- [ ] **Step 3: Run the sparql suites**

Run: `cargo test -p horndb-sparql` then `cargo test -p horndb-sparql --features server`
Expected: PASS — the `DELETE DATA`, delete-then-reinsert, and clear tests still pass through the native path.

- [ ] **Step 4: Clippy the crate**

Run: `cargo clippy -p horndb-sparql --all-targets --features server -- -D warnings`
Expected: no warnings (watch for now-unused imports/fields).

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/horn.rs crates/storage/src/store.rs
git commit -m "sparql: retire DELETE DATA tombstone overlay, delete through native storage retraction (SPEC-25 S1)"
```

---

## Task 10: Micro-bench, docs sync, plan close-out

**Files:**
- Create: `crates/storage/benches/insert_retract.rs`
- Modify: `crates/storage/Cargo.toml` (register the bench), `docs/architecture.md`, `crates/storage/INTEGRATION-NOTES.md`, `crates/storage/STAGE1-ACCEPTANCE.md`, `docs/benchmarks.md`, this plan file.

- [ ] **Step 1: Add the criterion micro-bench (local smoke only)**

Create `crates/storage/benches/insert_retract.rs` with two cases: `insert_10k` (baseline insert throughput, must not regress the insert-only path) and `retract_then_scan_10k` (retract 10% then scan, exercising the filter slow path). Follow the existing bench style in the crate (mirror `benches/` neighbours; register with a `[[bench]]` block in `Cargo.toml`, `harness = false`).

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

fn t(i: u64) -> (Term, Term, Term) {
    let n = |s: String| Term::NamedNode(NamedNode::new(s).unwrap());
    (
        n(format!("http://ex/s{i}")),
        n("http://ex/p".to_string()),
        n(format!("http://ex/o{i}")),
    )
}

fn bench(c: &mut Criterion) {
    let rows: Vec<_> = (0..10_000u64).map(t).collect();

    c.bench_function("insert_10k", |b| {
        b.iter(|| {
            let s = Store::in_memory();
            s.insert_triples(&rows).unwrap();
        })
    });

    c.bench_function("retract_then_scan_10k", |b| {
        b.iter(|| {
            let s = Store::in_memory();
            s.insert_triples(&rows).unwrap();
            s.retract_triples(&rows[..1_000]).unwrap();
            let snap = s.snapshot();
            std::hint::black_box(snap.len());
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

- [ ] **Step 2: Verify the bench builds and runs briefly**

Run: `cargo bench -p horndb-storage --bench insert_retract -- --warm-up-time 1 --measurement-time 2`
Expected: both cases complete. Do **not** record these numbers in `docs/benchmarks.md` (laptop run; hornbench is the recording host).

- [ ] **Step 3: Sync the docs**

- `docs/architecture.md`: flip the storage delete-path / per-tuple-MVCC Status row from `planned` → `implemented` (find the SPEC-02/SPEC-25 storage row; if only a "specified" S1 row exists, update it). Note the substrate choice (stamp columns on CoW) in one line.
- `crates/storage/INTEGRATION-NOTES.md`: add a short section — "Per-tuple MVCC (SPEC-25 S1)": stamp-columns-on-CoW substrate, `retract_quad_batch` semantics (one batch = one version, absent = counted no-op), the `begin ≤ v < end` read filter with the insert-only zero-copy fast path, `compact()` + pin registry, and the SPEC-24 S6 surface. State the deferred hornbench write-amp bench and its follow-up issue number.
- `crates/storage/STAGE1-ACCEPTANCE.md`: note that the delete path now exists (relevant to any row that assumed insertion-only).
- `docs/benchmarks.md`: add a row for the storage insert/retract micro-bench marked "measure on hornbench (deferred, #<follow-up>)". Do not invent numbers.
- This plan: flip `status: draft` → `status: executed` in the frontmatter.

- [ ] **Step 4: File the hornbench write-amp follow-up**

The spec's "bench stamp-columns vs delete-bitmap sidecars, CoW vs in-place append against NF4" is a perf-validation the design chose to defer (correctness gate acceptance #1 is met without it). File it:

```bash
gh issue create --repo sunstoneinstitute/horndb \
  --title "SPEC-25 S1 follow-up: hornbench write-amplification bench for the MVCC substrate (stamp-cols on CoW vs alternatives)" \
  --label "priority: medium" --label "category: performance" \
  --body "SPEC-25 S1 (#225) shipped per-tuple MVCC as stamp columns on the copy-on-write tier and met the correctness gate (acceptance #1). The spec also asks the substrate choice be benched against the NF4 write-amplification budget on hornbench, comparing stamp-columns-on-CoW vs delete-bitmap sidecars and CoW vs in-place-append. Run that comparison on hornbench and record it in docs/benchmarks.md; re-open the substrate decision only if CoW blows NF4. Parent: SPEC-25 epic #187."
```

Record the returned issue number in `INTEGRATION-NOTES.md`, `docs/benchmarks.md`, and (post-merge) as a new `TASKS.md` line.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/benches/insert_retract.rs crates/storage/Cargo.toml \
        docs/architecture.md crates/storage/INTEGRATION-NOTES.md \
        crates/storage/STAGE1-ACCEPTANCE.md docs/benchmarks.md \
        docs/plans/PLAN-25-01-per-tuple-mvcc-delete-path.md
git commit -m "storage(mvcc): micro-bench + docs sync + close PLAN-25-01 (SPEC-25 S1)"
```

---

## Self-review checklist (run before the first task lands)

**Spec coverage (SPEC-25 §S1 + acceptance #1):**
- Visibility stamps (begin/end on the commit clock) → Tasks 1, 2.
- Delete path (`retract_quad_batch`, batch atomicity, idempotent counted no-op) → Task 5.
- Version-consistent reads across all six orderings + Roaring sets + counts + export → Tasks 3, 4, 5, 7, 8.
- SPEC-24 S6 surface (`contains`, ordered `iter`, `len`/`is_empty`, `logical_time`) → Task 7.
- Compaction respecting pinned snapshots → Task 6.
- Retire the sparql `DELETE DATA` overlay → Task 9.
- Concurrency honesty (pinned-before sees, pinned-after doesn't) → Tasks 5, 8.
- Substrate choice benched → local smoke Task 10 + filed hornbench follow-up (deferred by design, acceptance #1 is correctness).

**Type consistency:** `CommitVersion`/`UNSET_END` (Task 1) used verbatim in Tasks 2–7. `append_stamped(s, o, begin, end)` signature stable across Tasks 2, 5, 6. `scan_at`/`ordered_at`/`len_at`/`subject_set_at`/`object_set_at`/`has_retractions`/`begins`/`ends` defined in Tasks 2–4 and called in 5–7. `PinnedSnapshot` (Task 6) is the `StoreSnapshot.tier` field type (Task 6 step 4) and Derefs to `TierSnapshot`. `retract_quad_batch -> Result<usize>` (Task 5) matches `Store::retract_triples -> Result<usize>` (Task 7).

**Harness-first (SPEC-00):** the SPEC-01 selected subset must stay green. Run `cargo test --workspace` and the selected harness suite in Phase 3 verification before opening the PR; the sparql delete tests are the closest conformance-facing guard.
