# SPEC-06 F7 — In-flight Reader Visibility (MVCC Snapshots) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a refcounted `Snapshot` handle to `Circuit` that pins a consistent `(asserted ∪ derived)` Z-set view at a logical time, surviving subsequent `tick()`s until dropped, with readers and writers never blocking each other.

**Architecture:** `Circuit` maintains a versioned materialized view as an `Arc<Zset<TripleId>>` (the sum of `asserted_base` and `derived_base`), rebuilt at the end of each state-changing `tick()`. `Circuit::snapshot()` returns a `Snapshot` holding an `Arc::clone` of the current version plus the logical time it represents — an O(1) acquire. Because each version Arc is immutable, a `tick()` swaps in a *new* Arc and existing snapshots keep reading the old one: writers never block readers and readers never block writers. This is the issue-#46 scope; full SPEC-02 per-tuple storage MVCC stays deferred under parent #6.

**Tech Stack:** Rust, `std::sync::Arc`, the crate's hand-rolled `Zset<K>`.

---

### Task 1: Versioned view + `Snapshot` type + `Circuit::snapshot()`

**Files:**
- Create: `crates/incremental/src/snapshot.rs`
- Modify: `crates/incremental/src/circuit.rs` (struct fields, `new()`, end of `tick()`, new method)
- Modify: `crates/incremental/src/lib.rs` (module + re-export)
- Test: `crates/incremental/tests/snapshot.rs`

- [ ] **Step 1: Write the failing test (empty snapshot + asserted view)**

Create `crates/incremental/tests/snapshot.rs`:

```rust
//! SPEC-06 F7 — in-flight reader visibility (MVCC snapshots).

use horndb_incremental::Circuit;

const P: u64 = 100;

#[test]
fn empty_circuit_snapshot_is_empty_at_time_zero() {
    let circuit = Circuit::new();
    let snap = circuit.snapshot();
    assert!(snap.is_empty());
    assert_eq!(snap.len(), 0);
    assert_eq!(snap.logical_time(), 0);
}

#[test]
fn snapshot_sees_asserted_rows_after_tick() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();

    let snap = circuit.snapshot();
    assert_eq!(snap.len(), 2);
    assert!(snap.contains(&(1, P, 2)));
    assert!(snap.contains(&(2, P, 3)));
    assert!(!snap.contains(&(9, P, 9)));
    assert_eq!(snap.get(&(1, P, 2)), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-incremental --test snapshot`
Expected: FAIL to compile — `no method named snapshot found for struct Circuit` (and `Snapshot`/its methods unresolved).

- [ ] **Step 3: Create the `Snapshot` type**

Create `crates/incremental/src/snapshot.rs`:

```rust
//! SPEC-06 F7 — in-flight reader visibility via MVCC snapshots.
//!
//! A [`Snapshot`] pins the materialized `(asserted ∪ derived)` Z-set view of a
//! [`crate::circuit::Circuit`] at the logical time it was acquired. It is
//! refcount-backed: acquiring clones an `Arc` (O(1)), and the pinned version is
//! immutable, so subsequent `tick()`s that publish a *new* version leave this
//! handle's view untouched until it is dropped. Readers (snapshot holders) and
//! writers (`tick`) therefore never block each other.
//!
//! Scope (issue #46): in-flight reader visibility within `horndb-incremental`.
//! Full SPEC-02 per-tuple storage MVCC stays deferred under parent #6.

use std::sync::Arc;

use crate::types::{LogicalTime, Multiplicity, TripleId};
use crate::zset::Zset;

/// A consistent, refcounted view of a [`Circuit`](crate::circuit::Circuit)'s
/// materialized state at a fixed logical time. Cheap to clone (Arc bump).
#[derive(Clone, Debug)]
pub struct Snapshot {
    time: LogicalTime,
    view: Arc<Zset<TripleId>>,
}

impl Snapshot {
    /// Construct a snapshot over an already-materialized version. Internal:
    /// callers go through [`Circuit::snapshot`](crate::circuit::Circuit::snapshot).
    pub(crate) fn new(time: LogicalTime, view: Arc<Zset<TripleId>>) -> Self {
        Self { time, view }
    }

    /// The logical time this snapshot represents: it reflects every asserted
    /// record with timestamp ≤ this value (SPEC-06 F7).
    pub fn logical_time(&self) -> LogicalTime {
        self.time
    }

    /// Multiplicity of `triple` in the pinned view (0 if absent).
    pub fn get(&self, triple: &TripleId) -> Multiplicity {
        self.view.get(triple)
    }

    /// Whether `triple` is present (non-zero multiplicity) in the pinned view.
    pub fn contains(&self, triple: &TripleId) -> bool {
        self.view.get(triple) != 0
    }

    /// Number of distinct triples in the pinned view.
    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// Whether the pinned view holds no triples.
    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Iterate `(triple, multiplicity)` pairs of the pinned view.
    pub fn iter(&self) -> impl Iterator<Item = (&TripleId, Multiplicity)> {
        self.view.iter()
    }
}
```

- [ ] **Step 4: Wire the module and re-export in `lib.rs`**

In `crates/incremental/src/lib.rs`, add the module declaration alongside the others (after `pub mod operator;`):

```rust
pub mod snapshot;
```

and add to the `pub use` block (after the `pub use operator::...;` line):

```rust
pub use snapshot::Snapshot;
```

Also add a bullet to the `# Module layout` doc comment, after the `[`circuit`]` bullet:

```rust
//! - [`snapshot`]: refcounted MVCC reader views pinned at a logical time (F7).
```

- [ ] **Step 5: Add versioned state to `Circuit` and the `snapshot()` method**

In `crates/incremental/src/circuit.rs`:

Add the import near the top (after the `use std::collections...` line):

```rust
use std::sync::Arc;
```

Add the `Snapshot` import to the existing `use crate::...` group:

```rust
use crate::snapshot::Snapshot;
```

Add two fields to `struct Circuit` (after `closure_support: BTreeSet<TripleId>,`):

```rust
    /// SPEC-06 F7 — current immutable materialized version, `asserted_base ∪
    /// derived_base`, shared with all live [`Snapshot`]s. A state-changing
    /// `tick()` replaces this Arc with a fresh one; snapshots holding the old
    /// Arc keep their pinned view (writers never block readers).
    version: Arc<Zset<TripleId>>,
    /// Logical time the current `version` represents: the max asserted-record
    /// timestamp merged so far (advances only on ticks that merge asserted
    /// records).
    version_time: LogicalTime,
```

Initialize them in `new()` (after `closure_support: BTreeSet::new(),`):

```rust
            version: Arc::new(Zset::new()),
            version_time: 0,
```

At the very end of `tick()`, *before* the `TickReport { ... }` return, rebuild the version when state changed:

```rust
        // SPEC-06 F7: publish a new immutable materialized version when this
        // tick changed state, so snapshots acquired afterwards see it and
        // snapshots acquired before keep their (now-superseded) Arc. Skip the
        // O(n) rebuild on no-op ticks. logical_time is 0 when no asserted
        // records were merged; only advance version_time on real progress.
        if asserted_merged > 0 || derived_merged > 0 {
            let mut materialized = self.asserted_base.clone();
            materialized.add_assign(&self.derived_base);
            self.version = Arc::new(materialized);
            if asserted_merged > 0 {
                self.version_time = logical_time;
            }
        }
```

Add the public method in the `impl Circuit` block (next to `asserted_base`/`derived_base` accessors):

```rust
    /// Acquire an MVCC [`Snapshot`] (SPEC-06 F7): a refcounted, consistent
    /// `(asserted ∪ derived)` view pinned at the current logical time. O(1) —
    /// it clones an `Arc` of the current version. The snapshot survives
    /// subsequent `tick()`s until dropped; readers and writers never block.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.version_time, Arc::clone(&self.version))
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p horndb-incremental --test snapshot`
Expected: PASS (`empty_circuit_snapshot_is_empty_at_time_zero`, `snapshot_sees_asserted_rows_after_tick`).

- [ ] **Step 7: Commit**

```bash
git add crates/incremental/src/snapshot.rs crates/incremental/src/circuit.rs crates/incremental/src/lib.rs crates/incremental/tests/snapshot.rs
git commit -m "feat(incremental): MVCC Snapshot handle pinning asserted∪derived view (SPEC-06 F7)"
```

---

### Task 2: Pinning across ticks + independent overlapping snapshots

**Files:**
- Test: `crates/incremental/tests/snapshot.rs` (append)

- [ ] **Step 1: Write the failing tests**

Append to `crates/incremental/tests/snapshot.rs`:

```rust
#[test]
fn snapshot_is_pinned_across_a_later_tick() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();

    let snap = circuit.snapshot();
    assert_eq!(snap.len(), 1);

    // A later tick adds a new triple. The pinned snapshot must NOT see it.
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    assert_eq!(snap.len(), 1, "snapshot must stay pinned across the tick");
    assert!(snap.contains(&(1, P, 2)));
    assert!(!snap.contains(&(3, P, 4)));

    // A fresh snapshot does see both.
    let fresh = circuit.snapshot();
    assert_eq!(fresh.len(), 2);
    assert!(fresh.contains(&(3, P, 4)));
}

#[test]
fn overlapping_snapshots_stay_independent() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();
    let s1 = circuit.snapshot();

    circuit.assert_triple((2, P, 3));
    circuit.tick();
    let s2 = circuit.snapshot();

    circuit.assert_triple((3, P, 4));
    circuit.tick();
    let s3 = circuit.snapshot();

    assert_eq!(s1.len(), 1, "s1 pinned at 1 triple");
    assert_eq!(s2.len(), 2, "s2 pinned at 2 triples");
    assert_eq!(s3.len(), 3, "s3 sees all 3");

    // Logical time advances across ticks that merge asserted records.
    assert!(s1.logical_time() < s2.logical_time());
    assert!(s2.logical_time() < s3.logical_time());
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p horndb-incremental --test snapshot`
Expected: PASS (all four tests). These exercise behaviour already implemented in Task 1; if either fails, fix the Task-1 version-rebuild logic rather than the test.

- [ ] **Step 3: Commit**

```bash
git add crates/incremental/tests/snapshot.rs
git commit -m "test(incremental): snapshot pinning + overlapping-independence (SPEC-06 F7)"
```

---

### Task 3: Derived rows are pinned in the snapshot

**Files:**
- Test: `crates/incremental/tests/snapshot.rs` (append)

- [ ] **Step 1: Write the failing test**

Append to `crates/incremental/tests/snapshot.rs` (note the added import — move it up to the existing `use` line):

```rust
use horndb_incremental::TransitiveClosureRule;

#[test]
fn snapshot_includes_and_pins_derived_rows() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // 1->2, 2->3 ⇒ transitive closure derives 1->3.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();

    let snap = circuit.snapshot();
    assert!(snap.contains(&(1, P, 2)), "asserted edge");
    assert!(snap.contains(&(2, P, 3)), "asserted edge");
    assert!(snap.contains(&(1, P, 3)), "derived transitive edge in snapshot");
    let pinned_len = snap.len();

    // Extend the chain; the derived 1->4/2->4/3->4 etc. must not leak into the
    // pinned snapshot.
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    assert_eq!(snap.len(), pinned_len, "derived rows stay pinned");
    assert!(!snap.contains(&(1, P, 4)), "new derived edge absent from old snap");

    let fresh = circuit.snapshot();
    assert!(fresh.contains(&(1, P, 4)), "fresh snapshot sees new derived edge");
}
```

Make sure the file has a single `use horndb_incremental::{Circuit, TransitiveClosureRule};` line at the top (merge the new import with the existing `use horndb_incremental::Circuit;`).

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p horndb-incremental --test snapshot`
Expected: PASS (`snapshot_includes_and_pins_derived_rows` plus the earlier tests).

- [ ] **Step 3: Commit**

```bash
git add crates/incremental/tests/snapshot.rs
git commit -m "test(incremental): snapshot pins derived (closure) rows (SPEC-06 F7)"
```

---

### Task 4: Reader/writer non-blocking property (concurrency)

**Files:**
- Test: `crates/incremental/tests/snapshot.rs` (append)

- [ ] **Step 1: Write the failing test**

Append to `crates/incremental/tests/snapshot.rs`:

```rust
use std::sync::mpsc;
use std::thread;

// NF4: readers do not block writers and writers do not block readers. A
// snapshot is Send + Sync (Arc-backed), so a reader thread can poll it
// concurrently with a writer thread driving ticks; the pinned view stays
// constant for the snapshot's whole lifetime.
#[test]
fn reader_does_not_block_writer_and_view_stays_stable() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();

    let snap = circuit.snapshot();
    let baseline = snap.len();
    let (tx, rx) = mpsc::channel();

    let reader = thread::spawn(move || {
        // Poll the pinned snapshot many times; it must never change.
        let mut observed = Vec::new();
        for _ in 0..10_000 {
            observed.push(snap.len());
        }
        tx.send(()).unwrap();
        observed
    });

    // Writer keeps ticking while the reader polls — must not block.
    for i in 0..2_000u64 {
        circuit.assert_triple((i + 10, P, i + 11));
        circuit.tick();
    }
    rx.recv().unwrap();
    let observed = reader.join().unwrap();

    assert!(
        observed.iter().all(|&n| n == baseline),
        "pinned snapshot len must stay constant under concurrent writes"
    );
    // The writer made progress concurrently.
    assert!(circuit.snapshot().len() > baseline);
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p horndb-incremental --test snapshot`
Expected: PASS. If it fails to compile with `Snapshot cannot be sent between threads safely`, that means `Zset<TripleId>` is not `Send + Sync`; it is (a `BTreeMap` of `Copy` keys/`i64` values), so a compile error here indicates an unintended non-`Send` field was added to `Snapshot` — fix `Snapshot`, not the test.

- [ ] **Step 3: Commit**

```bash
git add crates/incremental/tests/snapshot.rs
git commit -m "test(incremental): reader/writer non-blocking snapshot property (SPEC-06 F7/NF4)"
```

---

### Task 5: Documentation sync (crate notes + architecture Status)

**Files:**
- Modify: `crates/incremental/FUTURE-WORK.md` (§F7)
- Modify: `crates/incremental/CLAUDE.md`
- Modify: `docs/architecture.md` (SPEC-06 Status row)

> **Note:** Do NOT touch `TASKS.md` here. Under `/next-task` its `[v]`→release
> transition is a separate locked commit on `main` after merge.

- [ ] **Step 1: Update `FUTURE-WORK.md` §F7**

In `crates/incremental/FUTURE-WORK.md`, replace the `### F7` block's `**Stage 2**` bullet so it reflects that the in-process snapshot handle now exists. Change:

```
- **Stage 2**: arena-allocated `Snapshot` handles, refcounted; readers
  hold a `Snapshot` that pins a consistent view across multiple ticks.
  Intersects SPEC-02 MVCC design.
```

to:

```
- **Done (#46)**: refcounted `Snapshot` handles (`Circuit::snapshot()`,
  `crate::snapshot::Snapshot`) pin a consistent `(asserted ∪ derived)` view at
  a logical time across multiple ticks; readers and writers never block
  (Arc-versioned view, O(1) acquire).
- **Still deferred (parent #6)**: backing the snapshot interface onto SPEC-02
  per-tuple storage MVCC, and point queries against partially-applied in-flight
  deltas mid-tick.
```

- [ ] **Step 2: Update `crates/incremental/CLAUDE.md`**

In `crates/incremental/CLAUDE.md`, update the status bullet to note snapshots landed. Change the line:

```
- Stage 1 began **insertion-only**. Retraction is landing incrementally — F6
  retraction-across-joins has merged (#45). Full retraction + MVCC are tracked in
  task/issue #6. Treat the code as the source of truth for what currently works.
```

to:

```
- Stage 1 began **insertion-only**. Retraction is landing incrementally — F6
  retraction-across-joins has merged (#45) and F7 in-flight reader visibility
  via refcounted `Circuit::snapshot()` MVCC handles has merged (#46). Backing
  snapshots onto SPEC-02 storage MVCC is still tracked under task/issue #6.
  Treat the code as the source of truth for what currently works.
```

- [ ] **Step 3: Update `docs/architecture.md` SPEC-06 Status**

Find the SPEC-06 line covering MVCC / in-flight reader visibility. It currently marks MVCC as deferred/planned. Update the F7 entry's **Status** to `implemented` (in-process snapshot) and keep the SPEC-02-backed storage MVCC as deferred. Use `grep -n "F7\|MVCC\|reader visibility\|in-flight" docs/architecture.md` to locate the exact line, then edit it to read that in-flight reader visibility via refcounted `Circuit::snapshot()` is **implemented (#46)**, while SPEC-02-backed per-tuple MVCC stays **deferred**.

- [ ] **Step 4: Commit**

```bash
git add crates/incremental/FUTURE-WORK.md crates/incremental/CLAUDE.md docs/architecture.md docs/plans/2026-06-17-spec06-f7-mvcc-snapshots.md
git commit -m "docs(incremental): record F7 MVCC snapshots as implemented (#46)"
```

---

### Task 6: Full verification gate

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff (or apply the diff if any).

- [ ] **Step 2: Clippy (what CI runs)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean, no warnings.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS, including the new `snapshot` test target and all existing `horndb-incremental` tests (no regression in `circuit_tick`, `retraction`, `closure_deltas`, `acceptance_*`).

- [ ] **Step 4: Commit any fmt-only changes**

If `cargo fmt` produced changes:

```bash
git add -A
git commit -m "style(incremental): cargo fmt"
```
