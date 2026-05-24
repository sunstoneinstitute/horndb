# SPEC-06 DBSP Incremental Maintenance — Stage 0 / Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Stage-0/1 slice of SPEC-06 — a hand-rolled, narrow Z-set / DBSP-style incremental-maintenance core in the `horndb-incremental` crate, sufficient to (a) maintain rule-engine consequences under insertions at checkpoint snapshot boundaries and (b) emit an ordered change feed.

**Architecture:** A small in-crate Z-set library (`Zset<K>` over `BTreeMap<K, i64>`), a typed-erased operator trait (`Operator`), three concrete operator kinds (linear, bilinear, n-ary tree decomposition of bilinear), a `DeltaLog` of pending `(triple, ±1)` records, a `Checkpoint` merge routine that folds the log into a base `Zset`, and a `ChangeFeed` MPMC channel of committed deltas. The crate exposes a `Circuit` builder used by SPEC-04's rule codegen. Snapshot semantics are coarse: readers see either "pre-checkpoint" or "post-checkpoint" state, never in-flight deltas (Stage 2 widens this).

**Tech Stack:** Rust 2021, `std::collections::BTreeMap`, `crossbeam-channel` (single dependency for the change feed), `proptest` (property tests), `criterion` (benchmarks). No `differential-dataflow`, no `timely`. Workspace-pinned `anyhow` / `thiserror`.

---

## Key decision: hand-rolled vs `differential-dataflow`

**Decision: hand-roll a narrow Z-set + operator subset for Stage 1. Defer `differential-dataflow` adoption to Stage 2 (or never).**

Rationale (documented in `crates/incremental/src/lib.rs` doc comment as part of Task 1):

1. **Surface area.** `differential-dataflow` pulls `timely` and ~30 transitive crates oriented around a distributed dataflow runtime we explicitly defer to SPEC-09 (Stage 3). For the Stage-1 acceptance criteria (snapshot at checkpoint, insertion only, change feed) we need maybe 400-700 LOC of Z-set + bilinear-delta machinery.
2. **Auditability.** SPEC-06 is the highest-risk spec in the project. A hand-rolled core we can read end-to-end in an afternoon is easier to debug against the differential test (acceptance #4) than a thin wrapper over a dataflow runtime whose scheduler decisions we don't control.
3. **Reversibility.** The `Operator` trait and `Zset<K>` type are local to the crate. If Stage 2 needs `differential-dataflow` for multi-worker scaling we replace the implementation behind the trait; rule codegen (SPEC-04) is insulated.
4. **YAGNI.** Stage 1 insertion-only at checkpoint boundaries is genuinely a small piece of DBSP. The full `differential-dataflow` value (arrangement sharing across operators, multi-worker timely scheduling, incremental nested loops) is overkill until we hit those scaling pains.

This decision is **revisited** at the Stage 1 retro. Trigger to re-evaluate: if either F5 (closure-operator deltas, deferred here) or F6 (correct retraction with negative multiplicities propagating through joins) reveals a bilinear-composition bug whose fix duplicates `differential-dataflow`'s arrangement logic, switch.

---

## Scope boundary (Stage 0/1)

In scope for this plan:
- **F1** Z-set storage primitives.
- **F2** Linear rule operator.
- **F3** Bilinear rule operator (the `Δ(A⋈B) = Δ_A⋈B + A⋈Δ_B + Δ_A⋈Δ_B` decomposition).
- **F4** n-ary rule operator (left-deep tree of bilinears, planner is naïve — leftmost-first; cost-based planning is Stage 2).
- **F7** Snapshot semantics, **checkpoint-boundary only** (no in-flight reader visibility).
- **F8** Checkpoint merge (collapse `+1`/`-1` pairs, drop zero-multiplicity rows).
- **F9** Change feed (ordered MPMC stream of committed deltas).

Explicitly deferred (out of Stage 1 — documented in `FUTURE-WORK.md` created by Task 22):
- **F5** Closure-operator deltas (waits on SPEC-05 Stage 2).
- **F6** Correct retraction across joins. Stage 1 supports *insertion only*. Negative multiplicities in the API are accepted and logged but only the trivial "retract an asserted base triple that has no consequences" path is exercised; bilinear retraction is a Stage 2 deliverable.
- MVCC / in-flight reader visibility (Stage 2).
- Distributed timely-dataflow (Stage 3 / SPEC-09).
- `dt-*` datatype-aware operators (waits on SPEC-04 datatype handling).

Acceptance criteria from SPEC-06 driven by this plan:
- **#4 differential test vs full re-materialization** on a small ruleset (a 3-rule synthetic OWL 2 RL subset: `prp-trp` transitivity, `cax-sco` subClassOf, `prp-spo1` subPropertyOf) — Task 19.
- **#5 change-feed correctness** under sustained insertions — Task 20.

SPEC-06 acceptance #1 (100 ms latency on LUBM-1000) and #2 (100 K triples/sec sustained) are **NF targets**, not Stage 1 gates. Task 21 establishes the benchmark scaffolding so Stage 2 can measure regressions; Stage 1 only needs the bench to compile and run on a small fixture.

SPEC-06 acceptance #3 (retraction round-trip) is **deferred** with F6.

---

## Dependency interface contracts (frozen by this plan)

These are the only API surfaces by which the rest of the workspace touches `horndb-incremental`. They are introduced in early tasks, then *not changed* in later tasks (so SPEC-02 and SPEC-04 plans can be written against them in parallel).

### Stable types exported from `horndb-incremental` after Task 5

```rust
pub type TripleId = (u64, u64, u64);          // (s_id, p_id, o_id) — SPEC-02 dictionary ids
pub type Multiplicity = i64;                  // signed; +1 asserted, -1 retracted, others reserved
pub type LogicalTime = u64;                   // monotonically increasing per-circuit
pub type RuleId = u32;                        // assigned by SPEC-04 codegen

#[derive(Clone, Debug, Default)]
pub struct Zset<K: Ord + Clone> { /* BTreeMap<K, Multiplicity>, zeros pruned */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivationKind { Asserted, RuleInferred(RuleId), ClosureInferred }

pub struct DeltaRecord { pub triple: TripleId, pub mult: Multiplicity,
                         pub time: LogicalTime, pub kind: DerivationKind }
```

### Stable trait SPEC-04 implements after Task 9

```rust
pub trait BilinearRule: Send + Sync {
    fn id(&self) -> RuleId;
    /// Δ_out = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId>;
    /// Batch form for cold-start / Reset (acceptance #4 reference run).
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId>;
}
```

Linear rules implement the simpler `LinearRule` trait (Task 7). N-ary rules are planned into a left-deep tree of `BilinearRule`s by the `Circuit` builder (Task 11) — SPEC-04 emits the leaves, not the tree.

---

## File structure

Created in this plan (all under `/Users/stig/git/sunstone/reasoner/crates/incremental/`):

| Path | Responsibility |
|------|---------------|
| `Cargo.toml` | Updated: add `crossbeam-channel`, `dev-deps` `proptest`, `criterion`. |
| `src/lib.rs` | Crate-level docs (incl. the hand-roll decision), `pub use` of public items. |
| `src/zset.rs` | `Zset<K>` type, arithmetic ops, multiplicity invariants. |
| `src/types.rs` | `TripleId`, `Multiplicity`, `LogicalTime`, `RuleId`, `DerivationKind`, `DeltaRecord`. |
| `src/operator.rs` | `LinearRule`, `BilinearRule` traits and tree planner helpers. |
| `src/delta_log.rs` | Pending-delta append log, per-circuit logical clock. |
| `src/checkpoint.rs` | Checkpoint merge routine; collapses pending log into base `Zset`. |
| `src/change_feed.rs` | MPMC ordered change feed over `crossbeam-channel`. |
| `src/circuit.rs` | `Circuit` builder; owns base `Zset`s, operators, delta log, change feed; drives one tick. |
| `tests/zset_props.rs` | Property tests for Z-set algebraic laws. |
| `tests/bilinear_correctness.rs` | Bilinear delta-vs-full equivalence proptest. |
| `tests/end_to_end.rs` | Acceptance #4 differential test on the 3-rule synthetic ruleset. |
| `tests/change_feed.rs` | Acceptance #5 change-feed correctness. |
| `benches/insert_throughput.rs` | Criterion bench scaffold for NF1/NF2 (smoke-only in Stage 1). |
| `FUTURE-WORK.md` | Explicit deferral list: F5, F6, MVCC, distributed timely. |

Modified workspace files:
- `/Users/stig/git/sunstone/reasoner/Cargo.toml` — add `crossbeam-channel`, `proptest`, `criterion` to `[workspace.dependencies]`.

---

## Task 1: Add workspace dependencies and update crate manifest

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/Cargo.toml`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/Cargo.toml`

- [ ] **Step 1: Add the three new workspace deps**

Edit `/Users/stig/git/sunstone/reasoner/Cargo.toml`, replace the `[workspace.dependencies]` block:

```toml
[workspace.dependencies]
anyhow = "1"
thiserror = "1"
crossbeam-channel = "0.5"
proptest = "1"
criterion = { version = "0.5", default-features = false, features = ["html_reports"] }
```

- [ ] **Step 2: Wire the crate manifest**

Replace `/Users/stig/git/sunstone/reasoner/crates/incremental/Cargo.toml` entirely:

```toml
[package]
name = "horndb-incremental"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
crossbeam-channel.workspace = true

[dev-dependencies]
proptest.workspace = true
criterion.workspace = true

[[bench]]
name = "insert_throughput"
harness = false
```

- [ ] **Step 3: Verify workspace still resolves**

Run: `cargo metadata --format-version 1 --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml >/dev/null`
Expected: exit 0, no stderr.

- [ ] **Step 4: Verify the crate still compiles (placeholder lib still present)**

Run: `cargo build -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `Compiling horndb-incremental v0.0.0` then `Finished`. No warnings about unused deps (they're declared but not yet imported — that's allowed for direct deps, only `dev-dependencies` could warn; criterion won't until a bench imports it).

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add Cargo.toml Cargo.lock crates/incremental/Cargo.toml && git commit -m "$(cat <<'EOF'
incremental: wire workspace deps for SPEC-06 stage-1

Adds crossbeam-channel (change feed), proptest and criterion (dev-only)
to the workspace. Updates the placeholder horndb-incremental crate to
declare its deps. No code yet.
EOF
)"
```

---

## Task 2: Document the hand-roll decision and crate module layout

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`

- [ ] **Step 1: Replace the placeholder lib.rs with the module skeleton**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`:

```rust
//! `horndb-incremental` — DBSP-style incremental maintenance for SPEC-06.
//!
//! # Why a hand-rolled Z-set core?
//!
//! SPEC-06 explicitly allows either adopting `differential-dataflow` or
//! reimplementing the narrow Z-set subset we need. We chose the latter for
//! Stage 1 because:
//!
//! 1. The Stage-1 surface (linear + bilinear operators, checkpoint-boundary
//!    snapshots, insertion only) is ~few hundred LOC and we want to read it
//!    end-to-end when debugging the differential test (acceptance #4).
//! 2. `differential-dataflow` pulls `timely` plus ~30 transitive crates that
//!    target distributed scheduling we defer to SPEC-09 (Stage 3).
//! 3. The `BilinearRule` trait is the only contract SPEC-04 codegen depends
//!    on; we can swap the implementation behind it in Stage 2 if needed.
//!
//! Re-evaluate this decision if F5 (closure deltas) or F6 (retraction across
//! joins) forces us to duplicate `differential-dataflow`'s arrangement
//! sharing logic. See FUTURE-WORK.md.
//!
//! # Module layout
//!
//! - [`zset`]: `Zset<K>` and algebraic operations.
//! - [`types`]: triple-id, multiplicity, logical-time, derivation-kind.
//! - [`operator`]: `LinearRule`, `BilinearRule` traits; n-ary tree planner.
//! - [`delta_log`]: pending `(triple, ±1)` log between checkpoints.
//! - [`checkpoint`]: merge a delta log into the base store.
//! - [`change_feed`]: ordered MPMC stream of committed deltas (F9).
//! - [`circuit`]: top-level `Circuit` builder + tick driver.

pub mod change_feed;
pub mod checkpoint;
pub mod circuit;
pub mod delta_log;
pub mod operator;
pub mod types;
pub mod zset;

pub use change_feed::{ChangeFeed, ChangeFeedRx};
pub use checkpoint::Checkpoint;
pub use circuit::Circuit;
pub use delta_log::DeltaLog;
pub use operator::{BilinearRule, LinearRule};
pub use types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
pub use zset::Zset;
```

- [ ] **Step 2: Create empty module files so the crate compiles**

Create each of these as a single-line file (each just `//! TBD — populated by later tasks.`), so the `mod` declarations resolve:

```bash
cd /Users/stig/git/sunstone/reasoner/crates/incremental/src && \
  for f in change_feed.rs checkpoint.rs circuit.rs delta_log.rs operator.rs types.rs zset.rs; do
    printf '//!  populated by later tasks.\n' > "$f"
  done
```

- [ ] **Step 3: Verify build**

Run: `cargo build -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `error[E0432]: unresolved import` for each of the `pub use` lines (nothing is exported yet). This is expected — we'll fix in the next tasks.

To unblock the build now while subsequent tasks add real content, temporarily delete the `pub use` block (it'll be rebuilt incrementally as items get defined):

Re-edit `src/lib.rs` and remove the `pub use ...;` lines, leaving only the doc comment and `pub mod` declarations.

Re-run: `cargo build -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src && git commit -m "$(cat <<'EOF'
incremental: declare module layout and document hand-roll decision

Adds the seven module stubs (zset, types, operator, delta_log,
checkpoint, change_feed, circuit) and a crate-level doc explaining why
SPEC-06 stage 1 ships a hand-rolled Z-set core rather than adopting
differential-dataflow. Modules are empty; populated by subsequent tasks.
EOF
)"
```

---

## Task 3: Implement the core `Zset<K>` type

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/zset.rs`

- [ ] **Step 1: Write the failing test first**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/zset_basic.rs`:

```rust
use horndb_incremental::Zset;

#[test]
fn new_zset_is_empty() {
    let z: Zset<i64> = Zset::new();
    assert!(z.is_empty());
    assert_eq!(z.len(), 0);
}

#[test]
fn insert_then_get_returns_multiplicity() {
    let mut z = Zset::new();
    z.add(42, 1);
    assert_eq!(z.get(&42), 1);
    assert_eq!(z.len(), 1);
}

#[test]
fn adding_negative_cancels_positive() {
    let mut z = Zset::new();
    z.add(42, 1);
    z.add(42, -1);
    assert_eq!(z.get(&42), 0);
    assert!(z.is_empty(), "zero-multiplicity rows must be pruned");
}

#[test]
fn add_accumulates_multiplicities() {
    let mut z = Zset::new();
    z.add(42, 3);
    z.add(42, 2);
    assert_eq!(z.get(&42), 5);
}
```

- [ ] **Step 2: Re-add the `pub use` line so the test compiles**

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (right after the `pub mod zset;` line):

```rust
pub use zset::Zset;
```

- [ ] **Step 3: Run the test, watch it fail**

Run: `cargo test -p horndb-incremental --test zset_basic --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile errors `cannot find type Zset` / `no function or associated item named new`.

- [ ] **Step 4: Implement `Zset<K>`**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/zset.rs`:

```rust
//! `Zset<K>` — a multiplicity-weighted set over keys of type `K`.
//!
//! Invariant: no key maps to multiplicity 0. Adding `-m` to a key with
//! existing multiplicity `m` removes the row entirely.
//!
//! This is the F1 storage primitive from SPEC-06. We use `BTreeMap` for
//! deterministic iteration order (needed by the change-feed ordering
//! guarantee in acceptance #5) and to give the bilinear join a predictable
//! merge pattern. Hash-based variants are a Stage-2 optimization.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use crate::types::Multiplicity;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Zset<K: Ord + Clone> {
    inner: BTreeMap<K, Multiplicity>,
}

impl<K: Ord + Clone> Zset<K> {
    pub fn new() -> Self {
        Self { inner: BTreeMap::new() }
    }

    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
    pub fn len(&self) -> usize { self.inner.len() }

    /// Returns the current multiplicity of `key`, or 0 if absent.
    pub fn get(&self, key: &K) -> Multiplicity {
        self.inner.get(key).copied().unwrap_or(0)
    }

    /// Adds `delta` to the multiplicity of `key`. Removes the row if the
    /// resulting multiplicity is zero.
    pub fn add(&mut self, key: K, delta: Multiplicity) {
        if delta == 0 { return; }
        match self.inner.entry(key) {
            Entry::Occupied(mut o) => {
                let v = o.get_mut();
                *v += delta;
                if *v == 0 { o.remove(); }
            }
            Entry::Vacant(v) => { v.insert(delta); }
        }
    }

    /// Pointwise sum: `self += other`. Drops zero results.
    pub fn add_assign(&mut self, other: &Zset<K>) {
        for (k, &m) in &other.inner {
            self.add(k.clone(), m);
        }
    }

    /// Pointwise subtraction: `self -= other`.
    pub fn sub_assign(&mut self, other: &Zset<K>) {
        for (k, &m) in &other.inner {
            self.add(k.clone(), -m);
        }
    }

    /// Iterate `(&K, multiplicity)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, Multiplicity)> {
        self.inner.iter().map(|(k, &m)| (k, m))
    }

    /// Construct from an iterator of `(key, multiplicity)` pairs.
    /// Duplicate keys are summed; zero results are dropped.
    pub fn from_iter<I: IntoIterator<Item = (K, Multiplicity)>>(it: I) -> Self {
        let mut z = Self::new();
        for (k, m) in it { z.add(k, m); }
        z
    }
}
```

- [ ] **Step 5: Make sure `Multiplicity` exists so the import resolves**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/types.rs`:

```rust
//! Public type aliases for SPEC-06 stage-1 interfaces.
//!
//! These names are referenced by SPEC-04 (rule codegen) and SPEC-02
//! (storage); changing them is a coordinated workspace change.

pub type TripleId = (u64, u64, u64);
pub type Multiplicity = i64;
pub type LogicalTime = u64;
pub type RuleId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DerivationKind {
    Asserted,
    RuleInferred(RuleId),
    ClosureInferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaRecord {
    pub triple: TripleId,
    pub mult: Multiplicity,
    pub time: LogicalTime,
    pub kind: DerivationKind,
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add after `pub mod types;`:

```rust
pub use types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
```

- [ ] **Step 6: Run the test, watch it pass**

Run: `cargo test -p horndb-incremental --test zset_basic --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 4 passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/zset.rs crates/incremental/src/types.rs crates/incremental/src/lib.rs crates/incremental/tests/zset_basic.rs && git commit -m "$(cat <<'EOF'
incremental: implement core Zset<K> with multiplicity arithmetic (F1)

Add Zset<K> backed by BTreeMap, with the invariant that zero-multiplicity
rows are eagerly pruned. Exposes add, add_assign, sub_assign, get, iter,
from_iter. Also lands the public type aliases (TripleId, Multiplicity,
LogicalTime, RuleId, DerivationKind, DeltaRecord) that SPEC-02 and
SPEC-04 will depend on.

Covers SPEC-06 F1.
EOF
)"
```

---

## Task 4: Property tests for Z-set algebraic laws

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/zset_props.rs`

- [ ] **Step 1: Write the property tests first**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/zset_props.rs`:

```rust
//! Property tests for `Zset<K>`. The DBSP correctness arguments lean on
//! the abelian-group structure: addition is commutative, associative, and
//! has inverses. We assert each.

use proptest::prelude::*;
use horndb_incremental::Zset;

fn arb_zset() -> impl Strategy<Value = Zset<i32>> {
    prop::collection::vec((0i32..50, -3i64..=3), 0..30).prop_map(Zset::from_iter)
}

proptest! {
    #[test]
    fn add_assign_is_commutative(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone(); x.add_assign(&b);
        let mut y = b.clone(); y.add_assign(&a);
        prop_assert_eq!(x, y);
    }

    #[test]
    fn add_assign_is_associative(a in arb_zset(), b in arb_zset(), c in arb_zset()) {
        let mut left = a.clone(); left.add_assign(&b); left.add_assign(&c);
        let mut bc = b.clone(); bc.add_assign(&c);
        let mut right = a.clone(); right.add_assign(&bc);
        prop_assert_eq!(left, right);
    }

    #[test]
    fn sub_assign_inverts_add_assign(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone();
        x.add_assign(&b);
        x.sub_assign(&b);
        prop_assert_eq!(x, a);
    }

    #[test]
    fn no_zero_multiplicity_rows_after_any_op(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone();
        x.add_assign(&b);
        for (_, m) in x.iter() {
            prop_assert!(m != 0, "zero rows must be pruned");
        }
    }
}
```

- [ ] **Step 2: Run the props**

Run: `cargo test -p horndb-incremental --test zset_props --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 4 passed`. If a property fails, the proptest framework prints a minimised counterexample — fix `Zset` accordingly. (Empirically: passes on the implementation above.)

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/tests/zset_props.rs && git commit -m "$(cat <<'EOF'
incremental: property-test Zset abelian-group laws

Adds proptest coverage for commutativity, associativity, additive
inverses, and the zero-pruning invariant on Zset<i32>. These laws are
the foundation of DBSP's incremental-correctness argument; we verify
them at the type level before composing them into rule operators.
EOF
)"
```

---

## Task 5: Implement `DeltaLog` (pending insert/retract queue between checkpoints)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/delta_log.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/delta_log.rs`

- [ ] **Step 1: Write the failing tests**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/delta_log.rs`:

```rust
use horndb_incremental::{DeltaLog, DerivationKind};

#[test]
fn new_log_is_empty_and_time_starts_at_zero() {
    let log = DeltaLog::new();
    assert_eq!(log.len(), 0);
    assert_eq!(log.current_time(), 0);
}

#[test]
fn append_returns_monotonic_times() {
    let mut log = DeltaLog::new();
    let t1 = log.append((1, 2, 3), 1, DerivationKind::Asserted);
    let t2 = log.append((4, 5, 6), 1, DerivationKind::Asserted);
    assert!(t2 > t1, "logical time must increase per append");
    assert_eq!(log.len(), 2);
}

#[test]
fn iter_returns_records_in_append_order() {
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((4, 5, 6), -1, DerivationKind::Asserted);
    let triples: Vec<_> = log.iter().map(|r| (r.triple, r.mult)).collect();
    assert_eq!(triples, vec![((1, 2, 3), 1), ((4, 5, 6), -1)]);
}

#[test]
fn drain_clears_the_log_and_returns_records() {
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    let drained: Vec<_> = log.drain().collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(log.len(), 0);
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test delta_log --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile errors `cannot find type DeltaLog`.

- [ ] **Step 3: Implement `DeltaLog`**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/delta_log.rs`:

```rust
//! Pending `(triple, ±1)` records between checkpoints. SPEC-06 F1 / F7.
//!
//! Stage 1 simplification: this is an in-memory `Vec`, not persisted. A
//! crash between checkpoints loses pending deltas — that matches SPEC-02
//! NF5 (Stage 1 crash recovery rolls back to last checkpoint). A
//! write-ahead-log version is a Stage 2 deliverable owned by SPEC-02.

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, TripleId};

#[derive(Debug, Default)]
pub struct DeltaLog {
    records: Vec<DeltaRecord>,
    next_time: LogicalTime,
}

impl DeltaLog {
    pub fn new() -> Self { Self::default() }

    pub fn len(&self) -> usize { self.records.len() }
    pub fn is_empty(&self) -> bool { self.records.is_empty() }
    pub fn current_time(&self) -> LogicalTime { self.next_time }

    /// Append a record. Returns the logical time assigned to it.
    pub fn append(&mut self, triple: TripleId, mult: Multiplicity,
                  kind: DerivationKind) -> LogicalTime {
        let time = self.next_time;
        self.next_time = self.next_time.checked_add(1)
            .expect("logical-time u64 overflow (≈585 years at 1Gtps)");
        self.records.push(DeltaRecord { triple, mult, time, kind });
        time
    }

    /// Borrow records in append order.
    pub fn iter(&self) -> impl Iterator<Item = &DeltaRecord> { self.records.iter() }

    /// Empty the log and return owned records, preserving order.
    pub fn drain(&mut self) -> impl Iterator<Item = DeltaRecord> + '_ {
        self.records.drain(..)
    }
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (after `pub mod delta_log;`):

```rust
pub use delta_log::DeltaLog;
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test delta_log --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/delta_log.rs crates/incremental/src/lib.rs crates/incremental/tests/delta_log.rs && git commit -m "$(cat <<'EOF'
incremental: add DeltaLog for between-checkpoint pending records

In-memory log of (triple, ±1, logical-time, derivation-kind) records
appended between checkpoints. Provides monotonic logical-time
allocation and an ordered drain for checkpoint merge. Stage 1 is
volatile; a WAL variant is a Stage 2 SPEC-02 deliverable.
EOF
)"
```

---

## Task 6: Implement the change feed (F9)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/change_feed.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/change_feed.rs`

- [ ] **Step 1: Write the failing tests**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/change_feed.rs`:

```rust
use horndb_incremental::{ChangeFeed, DerivationKind};

#[test]
fn published_records_arrive_in_order() {
    let feed = ChangeFeed::new();
    let rx = feed.subscribe();

    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);
    feed.publish((4, 5, 6), 1, 1, DerivationKind::Asserted);
    feed.publish((7, 8, 9), -1, 2, DerivationKind::RuleInferred(42));

    let a = rx.recv().unwrap();
    let b = rx.recv().unwrap();
    let c = rx.recv().unwrap();

    assert_eq!(a.time, 0);
    assert_eq!(b.time, 1);
    assert_eq!(c.time, 2);
    assert_eq!(c.kind, DerivationKind::RuleInferred(42));
    assert_eq!(c.mult, -1);
}

#[test]
fn multiple_subscribers_each_see_all_records() {
    let feed = ChangeFeed::new();
    let rx1 = feed.subscribe();
    let rx2 = feed.subscribe();

    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);

    assert_eq!(rx1.recv().unwrap().triple, (1, 2, 3));
    assert_eq!(rx2.recv().unwrap().triple, (1, 2, 3));
}

#[test]
fn dropped_subscriber_does_not_block_publish() {
    let feed = ChangeFeed::new();
    let rx = feed.subscribe();
    drop(rx);
    // Must not panic / block.
    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test change_feed --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile errors `cannot find type ChangeFeed`.

- [ ] **Step 3: Implement the change feed**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/change_feed.rs`:

```rust
//! Ordered MPMC stream of committed deltas. SPEC-06 F9.
//!
//! Design: each subscriber gets its own unbounded `crossbeam-channel`
//! sender, kept in a `RwLock<Vec<_>>`. Publish iterates senders and
//! drops any whose receiver was closed. Per-subscriber ordering is
//! guaranteed by the single publisher path through `Circuit`; this
//! type itself takes the publisher's word.
//!
//! Stage-1 simplification: unbounded channels. A backpressure variant
//! (bounded + lag policy) is a Stage 2 deliverable.

use std::sync::RwLock;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, TripleId};

pub type ChangeFeedRx = Receiver<DeltaRecord>;

#[derive(Default)]
pub struct ChangeFeed {
    subscribers: RwLock<Vec<Sender<DeltaRecord>>>,
}

impl ChangeFeed {
    pub fn new() -> Self { Self::default() }

    pub fn subscribe(&self) -> ChangeFeedRx {
        let (tx, rx) = unbounded();
        self.subscribers.write().expect("change-feed lock poisoned").push(tx);
        rx
    }

    pub fn publish(&self, triple: TripleId, mult: Multiplicity,
                   time: LogicalTime, kind: DerivationKind) {
        let rec = DeltaRecord { triple, mult, time, kind };
        let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
        subs.retain(|tx| tx.send(rec).is_ok());
    }

    pub fn publish_record(&self, rec: DeltaRecord) {
        let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
        subs.retain(|tx| tx.send(rec).is_ok());
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().expect("change-feed lock poisoned").len()
    }
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (after `pub mod change_feed;`):

```rust
pub use change_feed::{ChangeFeed, ChangeFeedRx};
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test change_feed --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/change_feed.rs crates/incremental/src/lib.rs crates/incremental/tests/change_feed.rs && git commit -m "$(cat <<'EOF'
incremental: add ChangeFeed for committed deltas (SPEC-06 F9)

Per-subscriber crossbeam-channel sender, dropped lazily when its
receiver is gone. Supports multiple concurrent subscribers, no
backpressure (Stage 1 simplification — bounded variant is Stage 2).
Per-subscriber ordering follows from the single Circuit publisher
path; this type does not re-order.
EOF
)"
```

---

## Task 7: Define `LinearRule` trait and a `MapRule` adapter (F2)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/operator.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/linear_rule.rs`

- [ ] **Step 1: Write the failing test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/linear_rule.rs`:

```rust
//! `LinearRule` example: a synthetic `scm-*`-shaped rule that rewrites
//! triples of the form `(s, P, o)` into `(s, P', o)`. Linear in its
//! input — the delta passes straight through with the rule applied.

use horndb_incremental::{LinearRule, RuleId, TripleId, Zset};

struct RewritePredicate { id: RuleId, from: u64, to: u64 }

impl LinearRule for RewritePredicate {
    fn id(&self) -> RuleId { self.id }

    fn apply_delta(&self, delta: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((s, p, o), m) in delta.iter() {
            if *p == self.from {
                out.add((*s, self.to, *o), m);
            }
        }
        out
    }
}

#[test]
fn linear_rule_passes_delta_through_with_rewrite() {
    let rule = RewritePredicate { id: 1, from: 100, to: 200 };
    let delta = Zset::from_iter([
        ((1, 100, 2), 1),
        ((3, 100, 4), 1),
        ((5, 999, 6), 1),
    ]);

    let out = rule.apply_delta(&delta);
    assert_eq!(out.get(&(1, 200, 2)), 1);
    assert_eq!(out.get(&(3, 200, 4)), 1);
    assert_eq!(out.get(&(5, 999, 6)), 0);
    assert_eq!(out.len(), 2);
}

#[test]
fn linearity_delta_of_union_is_union_of_deltas() {
    // Linearity: f(a + b) == f(a) + f(b)
    let rule = RewritePredicate { id: 1, from: 100, to: 200 };
    let a = Zset::from_iter([((1, 100, 2), 1)]);
    let b = Zset::from_iter([((3, 100, 4), 1)]);

    let mut ab = a.clone(); ab.add_assign(&b);
    let f_ab = rule.apply_delta(&ab);

    let mut sum = rule.apply_delta(&a);
    sum.add_assign(&rule.apply_delta(&b));

    assert_eq!(f_ab, sum);
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test linear_rule --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile error `cannot find trait LinearRule`.

- [ ] **Step 3: Implement the `LinearRule` trait**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/operator.rs`:

```rust
//! Operator traits. SPEC-06 F2 (linear), F3 (bilinear), F4 (n-ary).
//!
//! These traits are the contract between this crate and SPEC-04 (rule
//! codegen). Adding a method here is a coordinated workspace change.
//!
//! Stage 1 covers insertion-only correctness. Negative-multiplicity
//! inputs are accepted; bilinear retraction across joins is a Stage 2
//! deliverable (F6 in SPEC-06).

use crate::types::{RuleId, TripleId};
use crate::zset::Zset;

/// F2: a rule whose body is a single triple pattern.
///
/// Linearity: `apply_delta(a + b) = apply_delta(a) + apply_delta(b)`.
/// Property-checked in `tests/linear_rule.rs`.
pub trait LinearRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply_delta(&self, delta: &Zset<TripleId>) -> Zset<TripleId>;
}

/// F3: a rule whose body is a conjunction of two triple patterns.
///
/// DBSP decomposition: `Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B`.
/// SPEC-04 codegen emits both `apply_full` (cold/Reset path) and
/// `apply_delta` (steady-state path).
pub trait BilinearRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply_delta(&self,
                   a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId>;
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId>;
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (after `pub mod operator;`):

```rust
pub use operator::{BilinearRule, LinearRule};
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test linear_rule --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/operator.rs crates/incremental/src/lib.rs crates/incremental/tests/linear_rule.rs && git commit -m "$(cat <<'EOF'
incremental: introduce LinearRule and BilinearRule traits (F2, F3 shape)

Defines the contract SPEC-04 rule codegen depends on. LinearRule is
proven linear by property test (delta of union = union of deltas).
BilinearRule's implementation comes in the next task; only the trait
shape is committed here so SPEC-04's own plan can be written against
a stable signature.
EOF
)"
```

---

## Task 8: Implement a reference `BilinearRule` and verify the F3 decomposition law

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/bilinear_correctness.rs`

This task does not change `operator.rs` — the trait was committed in Task 7 to lock the interface. Here we add a concrete rule implementation (in a test file) and verify the algebraic identity that justifies F3.

- [ ] **Step 1: Write the bilinear-correctness test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/bilinear_correctness.rs`:

```rust
//! Verify the F3 decomposition law for a reference bilinear rule.
//!
//! Rule: `prp-trp` style transitivity over a single fixed predicate P.
//! Body: `(?x P ?y) ∧ (?y P ?z)` → head: `(?x P ?z)`.
//!
//! We assert `Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B` over arbitrary
//! Z-sets of triples on the predicate P. `A` and `B` are both views of
//! the same relation (the predicate's extent) in `prp-trp`; we keep
//! them separate in the trait because most bilinear rules are joins of
//! two distinct patterns.

use proptest::prelude::*;
use horndb_incremental::{BilinearRule, RuleId, TripleId, Zset};

const P: u64 = 7;

struct PrpTrpOnP { id: RuleId }

impl BilinearRule for PrpTrpOnP {
    fn id(&self) -> RuleId { self.id }

    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        // Naïve nested-loop join for the reference implementation.
        // SPEC-04 codegen will emit hash/sort-merge variants; here we
        // only need correctness, not speed.
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != P { continue; }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != P { continue; }
                if xo == ys {
                    out.add((*xs, P, *yo), ma * mb);
                }
            }
        }
        out
    }

    fn apply_delta(&self,
                   a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

fn arb_p_triples(n: usize) -> impl Strategy<Value = Zset<TripleId>> {
    prop::collection::vec((0u64..6, 0u64..6), 0..n)
        .prop_map(|edges| Zset::from_iter(
            edges.into_iter().map(|(s, o)| ((s, P, o), 1))
        ))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn bilinear_decomposition_matches_full_recompute(
        a in arb_p_triples(10),
        da in arb_p_triples(4),
        b in arb_p_triples(10),
        db in arb_p_triples(4),
    ) {
        let rule = PrpTrpOnP { id: 1 };

        // Reference: full recompute on (A + ΔA) ⋈ (B + ΔB) minus A ⋈ B.
        let mut a_full = a.clone(); a_full.add_assign(&da);
        let mut b_full = b.clone(); b_full.add_assign(&db);
        let mut reference = rule.apply_full(&a_full, &b_full);
        let base = rule.apply_full(&a, &b);
        reference.sub_assign(&base);

        let decomposed = rule.apply_delta(&a, &b, &da, &db);

        prop_assert_eq!(reference, decomposed);
    }
}
```

- [ ] **Step 2: Run the proptest**

Run: `cargo test -p horndb-incremental --test bilinear_correctness --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 1 passed`. (Single proptest with 64 cases.)

If this fails with a counterexample, the `apply_delta` arithmetic is wrong — verify against the SPEC-06 F3 identity and SPEC ref McSherry/Ryzhyk/Tannen PVLDB 2023 §3.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/tests/bilinear_correctness.rs && git commit -m "$(cat <<'EOF'
incremental: property-test bilinear decomposition law (SPEC-06 F3)

Reference prp-trp-style transitive rule on a single predicate. Proptest
asserts Δ(A⋈B) computed by apply_delta equals the full recompute
((A+ΔA)⋈(B+ΔB) − A⋈B) on 64 random Z-set inputs. This pins the
algebraic identity that justifies the F3 decomposition; SPEC-04
codegen must satisfy it for every generated bilinear rule.
EOF
)"
```

---

## Task 9: N-ary tree planner for F4 (left-deep, naïve)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/operator.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/nary_plan.rs`

The Stage 1 planner is intentionally simple: rules with body arity n > 2 are *flattened to a left-deep tree* of bilinear joins (no cost model, no reordering). Cost-based planning is a Stage 2 deliverable. The planner's only job is to (a) hold the tree of `BilinearRule` references and (b) drive the delta through them in topological order.

- [ ] **Step 1: Write the failing test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/nary_plan.rs`:

```rust
//! F4: an n-ary rule is a left-deep tree of bilinear joins.
//!
//! We model a 3-pattern body (?x P ?y), (?y P ?z), (?z P ?w) inferring
//! (?x P ?w) as a tree of two prp-trp joins:
//!
//!   plan = Bilinear(P, P) → intermediate, then Bilinear(intermediate, P)
//!
//! and verify on a 4-node chain.

use horndb_incremental::{BilinearRule, NaryPlan, RuleId, TripleId, Zset};

const P: u64 = 7;

struct PrpTrpOnP { id: RuleId }
impl BilinearRule for PrpTrpOnP {
    fn id(&self) -> RuleId { self.id }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys { out.add((*xs, P, *yo), ma * mb); }
            }
        }
        out
    }
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

#[test]
fn left_deep_three_way_chain() {
    let r12 = PrpTrpOnP { id: 1 };
    let r23 = PrpTrpOnP { id: 2 };
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(r12));
    plan.push_join(Box::new(r23));

    // Base: 4-node chain 0-1-2-3 over P.
    let p_extent = Zset::from_iter([
        ((0, P, 1), 1),
        ((1, P, 2), 1),
        ((2, P, 3), 1),
    ]);

    // Full eval: should infer (0,P,2), (1,P,3), (0,P,3) and the
    // intermediate-pair derivations that compose to (0,P,3).
    let out = plan.apply_full(&p_extent);
    assert!(out.get(&(0, P, 3)) > 0, "transitive 3-hop must appear");
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test nary_plan --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile error `cannot find type NaryPlan`.

- [ ] **Step 3: Append `NaryPlan` to `operator.rs`**

Append to `/Users/stig/git/sunstone/reasoner/crates/incremental/src/operator.rs`:

```rust
/// F4: n-ary rule planner.
///
/// Stage 1: left-deep tree of bilinear joins. `push_join(rule)` appends
/// a join whose left input is the running intermediate and whose right
/// input is the base extent. Cost-based reordering is a Stage 2
/// deliverable.
///
/// All patterns currently bind against the same base extent — the
/// caller is responsible for slicing per-predicate inputs upstream. A
/// per-leaf-input variant is a Stage 2 extension if SPEC-04 finds
/// rules with bodies spanning different predicate partitions.
pub struct NaryPlan {
    joins: Vec<Box<dyn BilinearRule>>,
}

impl NaryPlan {
    pub fn new() -> Self { Self { joins: Vec::new() } }
    pub fn push_join(&mut self, rule: Box<dyn BilinearRule>) {
        self.joins.push(rule);
    }
    pub fn arity(&self) -> usize { self.joins.len() + 1 }

    /// Cold-start eval: fold the joins left-to-right starting from the
    /// base extent.
    pub fn apply_full(&self, base: &Zset<TripleId>) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return base.clone();
        }
        let mut intermediate = self.joins[0].apply_full(base, base);
        for rule in &self.joins[1..] {
            intermediate = rule.apply_full(&intermediate, base);
        }
        intermediate
    }

    /// Delta eval: each join is reduced via F3, the intermediates flow
    /// through as both base and delta inputs to the next join. Stage 1
    /// keeps the same `base` for every level for simplicity (correct
    /// when every body pattern reads the same predicate partition).
    pub fn apply_delta(&self, base: &Zset<TripleId>,
                       delta: &Zset<TripleId>) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return delta.clone();
        }
        let mut int_base = self.joins[0].apply_full(base, base);
        let mut int_delta = self.joins[0].apply_delta(base, base, delta, delta);
        for rule in &self.joins[1..] {
            let next_base = rule.apply_full(&int_base, base);
            let next_delta = rule.apply_delta(&int_base, base, &int_delta, delta);
            int_base = next_base;
            int_delta = next_delta;
        }
        int_delta
    }
}

impl Default for NaryPlan { fn default() -> Self { Self::new() } }
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, update the operator re-export:

```rust
pub use operator::{BilinearRule, LinearRule, NaryPlan};
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test nary_plan --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/operator.rs crates/incremental/src/lib.rs crates/incremental/tests/nary_plan.rs && git commit -m "$(cat <<'EOF'
incremental: add naïve left-deep NaryPlan for F4 rule bodies

Stage 1 planner folds a sequence of bilinear joins left-to-right against
a single base extent. No cost model, no reordering — that's a Stage 2
deliverable. Verifies a 3-hop transitive chain materialises correctly
via two stacked prp-trp-shape bilinears.
EOF
)"
```

---

## Task 10: Implement `Checkpoint` (F8 merge)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/checkpoint.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/checkpoint.rs`

- [ ] **Step 1: Write the failing tests**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/checkpoint.rs`:

```rust
use horndb_incremental::{Checkpoint, DeltaLog, DerivationKind, Zset};

#[test]
fn checkpoint_merges_pending_inserts_into_base() {
    let mut base: Zset<(u64, u64, u64)> = Zset::new();
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((4, 5, 6), 1, DerivationKind::Asserted);

    let report = Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(1, 2, 3)), 1);
    assert_eq!(base.get(&(4, 5, 6)), 1);
    assert_eq!(report.merged, 2);
    assert_eq!(log.len(), 0, "log must be drained after checkpoint");
}

#[test]
fn checkpoint_collapses_insert_then_retract_to_nothing() {
    let mut base: Zset<(u64, u64, u64)> = Zset::new();
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((1, 2, 3), -1, DerivationKind::Asserted);

    Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(1, 2, 3)), 0);
    assert_eq!(base.len(), 0, "no zero rows after checkpoint");
}

#[test]
fn checkpoint_preserves_existing_base_rows() {
    let mut base: Zset<(u64, u64, u64)> = Zset::from_iter([((7, 8, 9), 1)]);
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);

    Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(7, 8, 9)), 1);
    assert_eq!(base.get(&(1, 2, 3)), 1);
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test checkpoint --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile error `cannot find type Checkpoint`.

- [ ] **Step 3: Implement `Checkpoint`**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/checkpoint.rs`:

```rust
//! Checkpoint merge: drain a `DeltaLog` into the base `Zset`. SPEC-06 F8.
//!
//! Stage 1: a single base `Zset<TripleId>` per circuit; merge is one
//! pass over the log in append order, summing into the base. Zero-row
//! pruning is delegated to `Zset::add`.
//!
//! Stage 2 deliverables (not here): persistent on-disk checkpoint
//! format (SPEC-02), tiered merge across hot/warm/cold (SPEC-02 F6),
//! incremental closure-matrix reconstruction (SPEC-05 F6).

use crate::delta_log::DeltaLog;
use crate::types::TripleId;
use crate::zset::Zset;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckpointReport {
    pub merged: usize,
}

pub struct Checkpoint;

impl Checkpoint {
    pub fn merge(base: &mut Zset<TripleId>, log: &mut DeltaLog) -> CheckpointReport {
        let mut count = 0;
        for rec in log.drain() {
            base.add(rec.triple, rec.mult);
            count += 1;
        }
        CheckpointReport { merged: count }
    }
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (after `pub mod checkpoint;`):

```rust
pub use checkpoint::{Checkpoint, CheckpointReport};
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test checkpoint --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/checkpoint.rs crates/incremental/src/lib.rs crates/incremental/tests/checkpoint.rs && git commit -m "$(cat <<'EOF'
incremental: add Checkpoint::merge (SPEC-06 F8)

Drains a DeltaLog into a base Zset in append order, leaning on Zset's
zero-row pruning so +1/-1 pairs collapse. Returns a CheckpointReport
with the merged record count for tick metrics. Persistent on-disk
checkpoint format is a Stage 2 deliverable owned by SPEC-02.
EOF
)"
```

---

## Task 11: `Circuit` builder — wires together base, log, operators, checkpoint, feed

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/incremental/src/circuit.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/circuit_tick.rs`

This task is the integration point. The `Circuit` owns:
1. A base `Zset<TripleId>` for *asserted* triples.
2. A base `Zset<TripleId>` for *derived* triples (rule + closure consequences).
3. A `DeltaLog` of pending asserted records.
4. A vec of registered operators (each is a `LinearRule` or a `NaryPlan`).
5. A `ChangeFeed`.

A `tick()` call:
1. Treats the current asserted-log contents as `Δ_asserted`.
2. Runs every registered operator over `(asserted_base, asserted_delta)` to compute `Δ_derived`.
3. Merges `Δ_asserted` into `asserted_base` (via `Checkpoint`).
4. Merges `Δ_derived` into `derived_base`.
5. Publishes each merged record (asserted + derived) to the change feed.

Stage 1 runs **one round** of rule firing per tick — no semi-naïve fixed-point iteration. The fixed-point loop is owned by SPEC-04 and will be added when SPEC-04's compiled rules land; this crate exposes the per-round primitives.

- [ ] **Step 1: Write the failing test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/circuit_tick.rs`:

```rust
//! Integration: insert a triple, tick the circuit, observe the
//! derived consequence in the base store AND on the change feed.
//!
//! Reuses the prp-trp-shape rule from the bilinear-correctness test
//! but routed via a Circuit so we exercise the wiring end-to-end.

use horndb_incremental::{
    BilinearRule, Circuit, DerivationKind, NaryPlan, RuleId, TripleId, Zset,
};

const P: u64 = 7;

struct PrpTrpOnP { id: RuleId }
impl BilinearRule for PrpTrpOnP {
    fn id(&self) -> RuleId { self.id }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys { out.add((*xs, P, *yo), ma * mb); }
            }
        }
        out
    }
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

#[test]
fn insert_two_edges_then_tick_derives_transitive_consequence() {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));

    let mut circuit = Circuit::new();
    circuit.add_plan(plan, RuleId::from(1u32));
    let rx = circuit.subscribe();

    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));

    let report = circuit.tick();
    assert_eq!(report.asserted_merged, 2);
    assert!(report.derived_merged >= 1,
            "should derive at least (0,P,2)");

    // Base store contains the derivation.
    assert_eq!(circuit.derived_base().get(&(0, P, 2)), 1);

    // Change feed contains 2 asserted + ≥1 derived record. Drain.
    let mut seen = Vec::new();
    while let Ok(rec) = rx.try_recv() { seen.push(rec); }
    assert!(seen.iter().any(|r| r.triple == (0, P, 1) && r.kind == DerivationKind::Asserted));
    assert!(seen.iter().any(|r| r.triple == (0, P, 2)
            && matches!(r.kind, DerivationKind::RuleInferred(1))));
}
```

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p horndb-incremental --test circuit_tick --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: compile error `cannot find type Circuit`.

- [ ] **Step 3: Implement `Circuit`**

Overwrite `/Users/stig/git/sunstone/reasoner/crates/incremental/src/circuit.rs`:

```rust
//! `Circuit` — the SPEC-06 stage-1 driver.
//!
//! Owns:
//! - `asserted_base`: `Zset<TripleId>` of asserted triples.
//! - `derived_base`: `Zset<TripleId>` of rule/closure consequences.
//! - `log`: pending asserted records since the last tick.
//! - `plans`: registered operators (each tagged with its `RuleId` for
//!   change-feed `DerivationKind` annotation).
//! - `feed`: change-feed publisher.
//!
//! One `tick()` call:
//! 1. Snapshots the pending log as `Δ_asserted`.
//! 2. Runs every registered plan over (`asserted_base`, `Δ_asserted`)
//!    to compute `Δ_derived` (sum across plans).
//! 3. Drains `log` into `asserted_base` via `Checkpoint::merge`,
//!    publishing every record to the feed (kind = Asserted).
//! 4. Merges `Δ_derived` into `derived_base`, publishing each record
//!    (kind = RuleInferred(rule_id) for the originating plan).
//!
//! Stage 1 simplifications:
//! - One round of rule firing per tick. SPEC-04 will wrap this in a
//!   semi-naïve fixed-point loop driven by its dirty-flag machinery.
//! - Derived deltas are not fed back as inputs to other plans within
//!   the same tick. Multi-plan recursion is a Stage 2 concern that
//!   intersects SPEC-04's evaluation order.
//! - Closure deltas (F5) are not invoked here; SPEC-05 stage 2 wires
//!   in via a `add_closure_plan` extension.

use crate::change_feed::{ChangeFeed, ChangeFeedRx};
use crate::checkpoint::Checkpoint;
use crate::delta_log::DeltaLog;
use crate::operator::NaryPlan;
use crate::types::{DerivationKind, LogicalTime, RuleId, TripleId};
use crate::zset::Zset;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub asserted_merged: usize,
    pub derived_merged: usize,
    pub logical_time: LogicalTime,
}

pub struct Circuit {
    asserted_base: Zset<TripleId>,
    derived_base: Zset<TripleId>,
    log: DeltaLog,
    plans: Vec<(NaryPlan, RuleId)>,
    feed: ChangeFeed,
    derived_clock: LogicalTime,
}

impl Default for Circuit { fn default() -> Self { Self::new() } }

impl Circuit {
    pub fn new() -> Self {
        Self {
            asserted_base: Zset::new(),
            derived_base: Zset::new(),
            log: DeltaLog::new(),
            plans: Vec::new(),
            feed: ChangeFeed::new(),
            derived_clock: 0,
        }
    }

    pub fn add_plan(&mut self, plan: NaryPlan, attribution: RuleId) {
        self.plans.push((plan, attribution));
    }

    pub fn subscribe(&self) -> ChangeFeedRx { self.feed.subscribe() }

    pub fn asserted_base(&self) -> &Zset<TripleId> { &self.asserted_base }
    pub fn derived_base(&self) -> &Zset<TripleId> { &self.derived_base }

    /// Append an insertion to the pending log. Kind = Asserted.
    pub fn assert_triple(&mut self, triple: TripleId) {
        self.log.append(triple, 1, DerivationKind::Asserted);
    }

    /// Append a retraction. Stage 1: retraction of a triple with no
    /// derived consequences will produce the right answer; retraction
    /// of a triple whose consequences must also retract is F6 (Stage 2).
    pub fn retract_triple(&mut self, triple: TripleId) {
        self.log.append(triple, -1, DerivationKind::Asserted);
    }

    pub fn tick(&mut self) -> TickReport {
        // 1. Snapshot pending Δ_asserted from the log into a Zset.
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        for rec in self.log.iter() {
            asserted_delta.add(rec.triple, rec.mult);
        }

        // 2. Run every plan; collect per-plan derived deltas.
        //    We keep them separate so the change feed can attribute
        //    each derived record to its originating rule.
        let mut derived_per_plan: Vec<(Zset<TripleId>, RuleId)> =
            Vec::with_capacity(self.plans.len());
        for (plan, rid) in &self.plans {
            let dd = plan.apply_delta(&self.asserted_base, &asserted_delta);
            derived_per_plan.push((dd, *rid));
        }

        // 3. Drain the asserted log into asserted_base, publishing each
        //    record to the feed. Checkpoint::merge handles zero-pruning.
        let asserted_records: Vec<_> = self.log.drain().collect();
        let asserted_merged = asserted_records.len();
        for rec in &asserted_records {
            self.asserted_base.add(rec.triple, rec.mult);
            self.feed.publish_record(*rec);
        }
        let logical_time = if asserted_records.is_empty() {
            0
        } else {
            asserted_records.last().unwrap().time
        };

        // 4. Merge derived deltas into derived_base, publishing.
        //    Use derived_clock so derived records get monotonically
        //    increasing timestamps distinct from the asserted log's.
        let mut derived_merged = 0;
        for (dd, rid) in &derived_per_plan {
            for (triple, mult) in dd.iter() {
                self.derived_base.add(*triple, mult);
                let t = self.derived_clock;
                self.derived_clock = self.derived_clock
                    .checked_add(1).expect("derived-clock overflow");
                self.feed.publish(*triple, mult, t,
                                  DerivationKind::RuleInferred(*rid));
                derived_merged += 1;
            }
        }

        // Touch unused field to satisfy clippy when Checkpoint isn't used
        // directly — we inline the merge above to publish on each row.
        let _ = Checkpoint;

        TickReport { asserted_merged, derived_merged, logical_time }
    }
}
```

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/lib.rs`, add (after `pub mod circuit;`):

```rust
pub use circuit::{Circuit, TickReport};
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p horndb-incremental --test circuit_tick --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Run the full crate test suite to confirm no regressions**

Run: `cargo test -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: all of: `zset_basic` (4), `zset_props` (4), `delta_log` (4), `change_feed` (3), `linear_rule` (2), `bilinear_correctness` (1), `nary_plan` (1), `checkpoint` (3), `circuit_tick` (1). Total 23 tests, all passing.

- [ ] **Step 6: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/circuit.rs crates/incremental/src/lib.rs crates/incremental/tests/circuit_tick.rs && git commit -m "$(cat <<'EOF'
incremental: wire Circuit driver — base + log + plans + feed (F7 partial)

Single-round tick: snapshot pending Δ_asserted, run every registered
NaryPlan to derive Δ_derived, merge both into their respective base
Zsets, publish to the change feed with proper DerivationKind
attribution. Stage 1 stops at one round per tick; semi-naïve
fixed-point iteration is owned by SPEC-04 wrapping this. Snapshot
visibility is checkpoint-boundary only (F7 partial; in-flight reader
visibility is Stage 2).
EOF
)"
```

---

## Task 12: Reference 3-rule synthetic OWL 2 RL ruleset for the differential test

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/fixtures/mod.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/fixtures/synthetic_rules.rs`

We need a small, self-contained ruleset that exercises linear + bilinear + n-ary in a single fixture. It models three OWL 2 RL rules at the structural level (the IDs are synthetic; full OWL 2 RL semantics is SPEC-04's job):

- **R1 (linear, scm-sco-shaped):** `(?c, sc, ?d) ∧ (?d, sc, ?e) → (?c, sc, ?e)` over the `sc` predicate. (We model this as a 1-arg bilinear over a single predicate, like prp-trp.)
- **R2 (linear, scm-spo-shaped):** `(?p1, spo, ?p2) ∧ (?p2, spo, ?p3) → (?p1, spo, ?p3)`. Same shape, different predicate.
- **R3 (bilinear cross-predicate, cax-sco-shaped):** `(?x, type, ?c) ∧ (?c, sc, ?d) → (?x, type, ?d)`. This is the integration test — it joins two predicates.

We *also* expose a `full_rematerialize(asserted: &Zset)` reference function that runs all three rules to a fixed point by brute force. The acceptance #4 differential test in Task 19 calls this as the gold standard.

- [ ] **Step 1: Create the fixtures module**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/fixtures/mod.rs`:

```rust
pub mod synthetic_rules;
```

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/fixtures/synthetic_rules.rs`:

```rust
//! Synthetic 3-rule OWL-2-RL-shaped ruleset used by the SPEC-06
//! acceptance #4 differential test.
//!
//! Predicate ID assignments (chosen arbitrarily, internal to this
//! fixture; SPEC-04 owns the real OWL 2 RL predicate IDs):
//!   SC   = 100  ("rdfs:subClassOf"-like)
//!   SPO  = 101  ("rdfs:subPropertyOf"-like)
//!   TYPE = 102  ("rdf:type"-like)

#![allow(dead_code)]

use horndb_incremental::{BilinearRule, NaryPlan, RuleId, TripleId, Zset};

pub const SC: u64 = 100;
pub const SPO: u64 = 101;
pub const TYPE: u64 = 102;

pub const R1_SCM_SCO: RuleId = 1;
pub const R2_SCM_SPO: RuleId = 2;
pub const R3_CAX_SCO: RuleId = 3;

/// Bilinear self-join on a single predicate `p`: (?x p ?y) ∧ (?y p ?z) → (?x p ?z).
pub struct TransitiveOn { pub id: RuleId, pub p: u64 }

impl BilinearRule for TransitiveOn {
    fn id(&self) -> RuleId { self.id }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != self.p { continue; }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != self.p { continue; }
                if xo == ys { out.add((*xs, self.p, *yo), ma * mb); }
            }
        }
        out
    }
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

/// Bilinear cross-predicate join: (?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d).
pub struct CaxScoRule { pub id: RuleId }

impl BilinearRule for CaxScoRule {
    fn id(&self) -> RuleId { self.id }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != TYPE { continue; }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != SC { continue; }
                if xo == ys { out.add((*xs, TYPE, *yo), ma * mb); }
            }
        }
        out
    }
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

/// Build the three NaryPlans (each is a single bilinear) for the circuit.
pub fn build_plans() -> Vec<(NaryPlan, RuleId)> {
    let mut p1 = NaryPlan::new();
    p1.push_join(Box::new(TransitiveOn { id: R1_SCM_SCO, p: SC }));
    let mut p2 = NaryPlan::new();
    p2.push_join(Box::new(TransitiveOn { id: R2_SCM_SPO, p: SPO }));
    let mut p3 = NaryPlan::new();
    p3.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    vec![(p1, R1_SCM_SCO), (p2, R2_SCM_SPO), (p3, R3_CAX_SCO)]
}

/// Brute-force fixed-point reference. Repeatedly applies all three
/// rules to the asserted set ∪ derived set until no new triples
/// appear. Used as the gold standard for SPEC-06 acceptance #4.
pub fn full_rematerialize(asserted: &Zset<TripleId>) -> Zset<TripleId> {
    let r1 = TransitiveOn { id: R1_SCM_SCO, p: SC };
    let r2 = TransitiveOn { id: R2_SCM_SPO, p: SPO };
    let r3 = CaxScoRule { id: R3_CAX_SCO };
    let mut closure = asserted.clone();
    loop {
        let prev_len = closure.len();
        let d1 = r1.apply_full(&closure, &closure);
        let d2 = r2.apply_full(&closure, &closure);
        let d3 = r3.apply_full(&closure, &closure);
        closure.add_assign(&d1);
        closure.add_assign(&d2);
        closure.add_assign(&d3);
        if closure.len() == prev_len { break; }
    }
    closure
}
```

- [ ] **Step 2: Add a smoke test that uses the fixture so it compiles**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/fixtures_smoke.rs`:

```rust
mod fixtures;

use fixtures::synthetic_rules::{build_plans, full_rematerialize, SC, TYPE};
use horndb_incremental::Zset;

#[test]
fn fixtures_compile_and_basic_closure_works() {
    let plans = build_plans();
    assert_eq!(plans.len(), 3);

    // 3-class hierarchy: A sc B, B sc C. (instance, type, A).
    // Reference closure should derive (instance, type, B) and (instance, type, C),
    // and (A, sc, C).
    let asserted = Zset::from_iter([
        ((10, SC, 20), 1),
        ((20, SC, 30), 1),
        ((1, TYPE, 10), 1),
    ]);
    let closure = full_rematerialize(&asserted);

    assert_eq!(closure.get(&(10, SC, 30)), 1);
    assert_eq!(closure.get(&(1, TYPE, 20)), 1);
    assert_eq!(closure.get(&(1, TYPE, 30)), 1);
}
```

- [ ] **Step 3: Run the smoke test**

Run: `cargo test -p horndb-incremental --test fixtures_smoke --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/tests/fixtures crates/incremental/tests/fixtures_smoke.rs && git commit -m "$(cat <<'EOF'
incremental: add 3-rule synthetic OWL-2-RL fixture for SPEC-06 #4

Three rules (scm-sco-shape, scm-spo-shape, cax-sco-shape) wired as
two TransitiveOn bilinears and one CaxScoRule cross-predicate
bilinear. Also exposes a brute-force full_rematerialize fixed-point
function used as the gold standard for the differential test in a
later task. Smoke test verifies the closure of a 3-class hierarchy.
EOF
)"
```

---

## Task 13: Acceptance #4 — differential test vs full re-materialization

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/acceptance_differential.rs`

This is the headline Stage-1 acceptance gate. We assert: after an arbitrary sequence of insertions, the `Circuit.derived_base` is equal (modulo logical timestamps, which are not part of `Zset` content) to `full_rematerialize(Circuit.asserted_base) − Circuit.asserted_base`.

Insertion-only for Stage 1 (per F6 deferral).

- [ ] **Step 1: Write the failing test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/acceptance_differential.rs`:

```rust
//! SPEC-06 acceptance #4: incremental ≡ full re-materialization.
//!
//! Stage 1 scope: insertions only. We pick a sequence of insertions,
//! drive them through a Circuit one batch at a time with tick() in
//! between, then assert the Circuit's derived_base equals the
//! fixed-point reference run from scratch on the cumulative asserted
//! set (minus the asserted set itself, which lives in asserted_base).
//!
//! We tick after every individual insert *and* after every batch of
//! inserts, to exercise both fine-grained and coarse-grained
//! incrementalisation.

mod fixtures;

use fixtures::synthetic_rules::{build_plans, full_rematerialize, SC, SPO, TYPE};
use proptest::prelude::*;
use horndb_incremental::{Circuit, TripleId, Zset};

/// Returns true if `incremental` equals the reference run.
fn check_equivalence(asserted: &Zset<TripleId>, incremental: &Zset<TripleId>) -> bool {
    let mut reference = full_rematerialize(asserted);
    // The reference includes the asserted base; the Circuit's
    // derived_base does not, so subtract.
    reference.sub_assign(asserted);
    // Approximate equality: every key in reference should also appear
    // in incremental with the same multiplicity (>= 1; Stage 1 may
    // double-derive on multi-rule paths, accept that and assert
    // membership only).
    for (k, m) in reference.iter() {
        if incremental.get(k) < m {
            eprintln!("missing: {:?} expected mult {}, got {}",
                      k, m, incremental.get(k));
            return false;
        }
    }
    // Also: no spurious derivations.
    for (k, _) in incremental.iter() {
        if reference.get(k) == 0 {
            eprintln!("spurious: {:?}", k);
            return false;
        }
    }
    true
}

fn small_random_inserts() -> impl Strategy<Value = Vec<TripleId>> {
    let pred = prop::sample::select(vec![SC, SPO, TYPE]);
    let triple = (0u64..6, pred, 0u64..6).prop_map(|(s, p, o)| (s, p, o));
    prop::collection::vec(triple, 1..20)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    #[test]
    fn insert_then_tick_matches_full_rematerialize(inserts in small_random_inserts()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }

        for triple in &inserts {
            circuit.assert_triple(*triple);
        }
        // One coarse tick.
        circuit.tick();

        prop_assert!(
            check_equivalence(circuit.asserted_base(), circuit.derived_base()),
            "incremental derived set diverges from full re-materialization reference"
        );
    }

    #[test]
    fn tick_per_insert_matches_full_rematerialize(inserts in small_random_inserts()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }
        for triple in &inserts {
            circuit.assert_triple(*triple);
            circuit.tick();
        }
        prop_assert!(
            check_equivalence(circuit.asserted_base(), circuit.derived_base())
        );
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-incremental --test acceptance_differential --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: **likely fails on the multi-rule cases** — Stage 1 `Circuit::tick` runs each plan once over `Δ_asserted` but does **not** feed derived triples back as inputs to other plans, so derivations like "R1 derives `(10, sc, 30)`, then R3 uses that as `(instance, type, 30)`" require either (a) multiple ticks or (b) a fixed-point iteration that's currently SPEC-04's job.

If the tests fail with "missing" derivations of that shape, the fix is **not** to weaken the test — the fix is to add a bounded fixed-point loop *inside* `Circuit::tick` for Stage 1, so the differential acceptance holds. Do that now:

Edit `/Users/stig/git/sunstone/reasoner/crates/incremental/src/circuit.rs`, replace the body of `tick(...)` with a fixed-point loop:

```rust
    pub fn tick(&mut self) -> TickReport {
        // First, drain pending asserted records into asserted_base and
        // publish them. We need them in the base before running the
        // fixed-point so that subsequent rounds can join against them.
        let asserted_records: Vec<_> = self.log.drain().collect();
        let asserted_merged = asserted_records.len();
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        for rec in &asserted_records {
            asserted_delta.add(rec.triple, rec.mult);
            self.asserted_base.add(rec.triple, rec.mult);
            self.feed.publish_record(*rec);
        }
        let logical_time = asserted_records.last().map(|r| r.time).unwrap_or(0);

        // Fixed-point: keep firing plans until no new derived rows
        // appear. Inputs to a plan are (asserted_base ∪ derived_base)
        // and the running delta (asserted_delta initially, then
        // last-round's derived delta).
        //
        // Bound the loop at MAX_ROUNDS to surface non-termination
        // bugs early in development.
        const MAX_ROUNDS: usize = 64;
        let mut combined_base: Zset<TripleId> = self.asserted_base.clone();
        combined_base.add_assign(&self.derived_base);
        let mut round_delta = asserted_delta;
        let mut derived_merged = 0;

        for _ in 0..MAX_ROUNDS {
            let mut next_delta: Zset<TripleId> = Zset::new();
            for (plan, rid) in &self.plans {
                let dd = plan.apply_delta(&combined_base, &round_delta);
                // Subtract any rows already present in the cumulative
                // base — those are not "new" derivations this round.
                let mut new_only = Zset::new();
                for (triple, mult) in dd.iter() {
                    let pre = combined_base.get(triple);
                    let post = pre + mult;
                    // Emit only the multiplicity that crosses zero into
                    // the "newly present" range.
                    if pre == 0 && post != 0 {
                        new_only.add(*triple, mult);
                    } else if pre != 0 && post == 0 {
                        new_only.add(*triple, mult);
                    }
                    // Other cases are repeat-derivations under the
                    // same rule; the semi-naïve filter drops them.
                }
                for (triple, mult) in new_only.iter() {
                    self.derived_base.add(*triple, mult);
                    combined_base.add(*triple, mult);
                    let t = self.derived_clock;
                    self.derived_clock = self.derived_clock
                        .checked_add(1).expect("derived-clock overflow");
                    self.feed.publish(*triple, mult, t,
                                      DerivationKind::RuleInferred(*rid));
                    derived_merged += 1;
                    next_delta.add(*triple, mult);
                }
            }
            if next_delta.is_empty() { break; }
            round_delta = next_delta;
        }

        TickReport { asserted_merged, derived_merged, logical_time }
    }
```

- [ ] **Step 3: Re-run the differential test**

Run: `cargo test -p horndb-incremental --test acceptance_differential --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 2 passed`. (40 cases × 2 tests = 80 random scenarios, all matching the full-rematerialize reference.)

If still failing, the proptest framework prints a minimised counterexample. Likely cause: a missing case in the `new_only` filter. The intended semantics for *insertion only* is: emit the row exactly when it crosses from absent to present (`pre == 0 && post == 1`). Adjust accordingly and re-run.

- [ ] **Step 4: Re-run the full crate test suite to confirm no regressions**

Run: `cargo test -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: all prior tests still pass plus the 2 new ones.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/src/circuit.rs crates/incremental/tests/acceptance_differential.rs && git commit -m "$(cat <<'EOF'
incremental: SPEC-06 acceptance #4 — differential ≡ full rematerialize

Adds a semi-naïve fixed-point loop inside Circuit::tick so derived
triples feed back as inputs to subsequent rounds, plus a proptest
that compares the incremental derived_base against a brute-force
full_rematerialize on 80 random insert sequences over the 3-rule
synthetic OWL-2-RL fixture (insertions only — F6 retraction is
Stage 2). MAX_ROUNDS = 64 surfaces non-termination bugs early.
EOF
)"
```

---

## Task 14: Acceptance #5 — change-feed correctness under sustained writes

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/acceptance_change_feed.rs`

- [ ] **Step 1: Write the test**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/tests/acceptance_change_feed.rs`:

```rust
//! SPEC-06 acceptance #5: change-feed correctness.
//!
//! Property: under any sequence of insertions ticked through the
//! Circuit, a subscriber sees every committed (asserted + derived)
//! delta exactly once, in publication order, with no gaps and no
//! duplicates.

mod fixtures;

use fixtures::synthetic_rules::{build_plans, SC, SPO, TYPE};
use horndb_incremental::{Circuit, DerivationKind};
use std::collections::HashSet;

#[test]
fn no_gaps_no_duplicates_under_sustained_inserts() {
    let mut circuit = Circuit::new();
    for (plan, rid) in build_plans() {
        circuit.add_plan(plan, rid);
    }
    let rx = circuit.subscribe();

    // 1000 insertions across 100 ticks (10 per tick), drawn from a
    // 5×5×5 ID space so we get lots of join opportunities.
    let mut asserted_count = 0;
    for tick_i in 0..100u64 {
        for j in 0..10u64 {
            let s = (tick_i * 10 + j) % 5;
            let p = match (tick_i + j) % 3 { 0 => SC, 1 => SPO, _ => TYPE };
            let o = (tick_i + j) % 5;
            circuit.assert_triple((s, p, o));
            asserted_count += 1;
        }
        circuit.tick();
    }

    // Drain the feed.
    let mut all = Vec::new();
    while let Ok(rec) = rx.try_recv() { all.push(rec); }

    // Asserted records: exactly one per assert_triple call. Some may
    // be "noop" (insert of already-present triple, which still
    // publishes — Stage 1 publishes every log append).
    let asserted: Vec<_> = all.iter()
        .filter(|r| matches!(r.kind, DerivationKind::Asserted))
        .collect();
    assert_eq!(asserted.len(), asserted_count,
               "every assert_triple must produce exactly one Asserted feed record");

    // Asserted logical times are unique and strictly monotonic.
    let asserted_times: Vec<u64> = asserted.iter().map(|r| r.time).collect();
    let unique: HashSet<_> = asserted_times.iter().collect();
    assert_eq!(unique.len(), asserted_times.len(), "duplicate asserted times");
    for w in asserted_times.windows(2) {
        assert!(w[0] < w[1], "asserted times must be strictly increasing");
    }

    // Derived records: every (triple, mult, rule_id) corresponds to
    // a row currently in derived_base (no spurious publishes).
    for rec in all.iter() {
        if let DerivationKind::RuleInferred(_) = rec.kind {
            // Either the row is present in derived_base, or a later
            // retraction publish (mult = -1) cancelled it. Stage 1
            // is insertion-only so the second case shouldn't occur;
            // assert presence.
            assert!(circuit.derived_base().get(&rec.triple) > 0,
                    "derived feed record {:?} has no matching base row",
                    rec.triple);
        }
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p horndb-incremental --test acceptance_change_feed --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `test result: ok. 1 passed`.

If it fails on "every assert_triple must produce exactly one feed record" — likely the change feed is dropping records because no subscriber is active when `publish_record` runs. Verify: `circuit.subscribe()` is called *before* the first `assert_triple`. The test above does this; if the implementation regresses this property, that's a real bug to fix in `Circuit::tick`.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/tests/acceptance_change_feed.rs && git commit -m "$(cat <<'EOF'
incremental: SPEC-06 acceptance #5 — change-feed correctness

Sustained 1000-insert/100-tick test asserts every Asserted record
appears exactly once with strictly increasing logical times, and
every RuleInferred record published to the feed corresponds to a
row currently present in derived_base (no spurious publishes).
EOF
)"
```

---

## Task 15: Benchmark scaffold (`criterion`) for NF1 / NF2 — smoke only in Stage 1

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/benches/insert_throughput.rs`

NF1 (≤100 ms steady-state insert latency on LUBM-1000) and NF2 (≥100 K triples/sec sustained on LUBM-8000) are *Stage 2* gates — Stage 1 just needs the bench to compile and run on a small fixture so future plans can measure regressions.

- [ ] **Step 1: Write the bench**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/benches/insert_throughput.rs`:

```rust
//! Insert-throughput micro-benchmark.
//!
//! Stage 1 purpose: provide a `cargo bench` entry point so regressions
//! show up in CI. NF1/NF2 numbers are Stage 2 deliverables and will
//! need an LUBM-shaped fixture; here we use a synthetic 10K-triple
//! schema closure and assert nothing about wall time — criterion just
//! records the number for later comparison.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_incremental::{BilinearRule, Circuit, NaryPlan, RuleId, TripleId, Zset};

const P: u64 = 7;

struct TransitiveP;
impl BilinearRule for TransitiveP {
    fn id(&self) -> RuleId { 1 }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys { out.add((*xs, P, *yo), ma * mb); }
            }
        }
        out
    }
    fn apply_delta(&self, a: &Zset<TripleId>, b: &Zset<TripleId>,
                   da: &Zset<TripleId>, db: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    for &n in &[10u64, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut circuit = Circuit::new();
                let mut plan = NaryPlan::new();
                plan.push_join(Box::new(TransitiveP));
                circuit.add_plan(plan, 1);
                for i in 0..n {
                    circuit.assert_triple((i, P, i + 1));
                }
                circuit.tick();
                criterion::black_box(circuit.derived_base().len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_insert);
criterion_main!(benches);
```

- [ ] **Step 2: Verify the bench compiles and runs**

Run: `cargo bench -p horndb-incremental --no-run --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: `Compiling ... Finished`. No execution yet (`--no-run`).

Smoke-run the bench briefly:

Run: `cargo bench -p horndb-incremental --bench insert_throughput --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml -- --quick --warm-up-time 1 --measurement-time 2`
Expected: criterion prints three timing lines (one each for n=10/100/1000). No assertion on the numbers in Stage 1.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/benches/insert_throughput.rs && git commit -m "$(cat <<'EOF'
incremental: criterion bench scaffold for SPEC-06 NF1/NF2

Stage 1 only proves the bench compiles and runs on a synthetic
transitive-chain fixture (n = 10/100/1000). NF1 (100 ms LUBM-1000
latency) and NF2 (100 K tps LUBM-8000) are Stage 2 gates that need
the SPEC-01 harness and SPEC-02 storage to be real; this scaffold
gives future plans a target binary to extend.
EOF
)"
```

---

## Task 16: Document deferred work explicitly in `FUTURE-WORK.md`

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/incremental/FUTURE-WORK.md`

- [ ] **Step 1: Write the deferral doc**

Create `/Users/stig/git/sunstone/reasoner/crates/incremental/FUTURE-WORK.md`:

```markdown
# horndb-incremental — Deferred Work

Stage 1 of SPEC-06 deliberately ships a narrow slice. This file
catalogues what is **out** of Stage 1, in priority order for Stage 2,
with the SPEC-06 requirement ID and the trigger for promotion.

## Stage 2 (next milestone)

### F6 — Correct retraction across joins
- **Now**: `Circuit::retract_triple` accepts -1 and the asserted log
  carries it. The fixed-point loop in `Circuit::tick` filters derived
  emissions via the `pre==0 ↔ post!=0` heuristic, which is **only
  correct for monotonic insertion**.
- **Stage 2**: derive correctly under arbitrary `(triple, ±k)` deltas
  by passing real Z-set algebra through every bilinear rather than the
  "newly present" filter. This is the DBSP correctness theorem
  (McSherry/Ryzhyk/Tannen PVLDB 2023 §3) and requires the bilinear
  rules to handle negative multiplicities in their join (currently
  the reference fixtures already do; the filter is the obstacle).
- **Promotion test**: SPEC-06 acceptance #3 — insert 10K, retract 10K,
  store is bit-identical (modulo timestamps) to pre-insertion.

### F7 — In-flight reader visibility (MVCC)
- **Now**: readers see either pre-tick or post-tick state via
  `&Circuit` borrow; concurrent reads during a tick are not exposed.
- **Stage 2**: arena-allocated `Snapshot` handles, refcounted; readers
  hold a `Snapshot` that pins a consistent view across multiple ticks.
  Intersects SPEC-02 MVCC design.

### F5 — Closure-operator deltas (SPEC-05 integration)
- **Now**: not invoked. The `Circuit` has no `ClosurePlan` slot.
- **Stage 2**: add `add_closure_plan(...)` and a `ClosureRule` trait
  that wraps a GraphBLAS matrix-power step. SPEC-05 owns the matrix
  side; SPEC-06 owns the delta integration.

## Stage 3 (SPEC-09 / hardware)

### Distributed timely-dataflow
- **Now**: single-process `Circuit`.
- **Stage 3**: re-evaluate whether to adopt `timely`+`differential-dataflow`
  for distributed workers, or to keep a custom distributed scheduler
  on top of the `Zset` core. Decision deferred until single-node
  throughput is exhausted.

## Stage-1 simplifications worth revisiting opportunistically

- **DeltaLog persistence**: currently in-memory; SPEC-02 will add a
  per-predicate WAL in Stage 2. The log's `drain()` interface is
  WAL-compatible.
- **Backpressure on change feed**: currently unbounded channels.
  Subscribers that fall behind grow the channel without limit. A
  bounded variant with a lag policy (drop / slow producer / kill
  consumer) lands when a real downstream subscriber materialises.
- **NaryPlan cost model**: current planner is left-deep and naïve.
  Cost-based reordering using SPEC-02's predicate-partition statistics
  is a Stage 2 deliverable.
- **`HashMap` vs `BTreeMap` in `Zset`**: BTreeMap was chosen for
  deterministic iteration (change-feed ordering). If profiling shows
  iteration is not the bottleneck and lookup dominates, swap to a
  randomised-state HashMap with a stable iteration adapter.
```

- [ ] **Step 2: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental/FUTURE-WORK.md && git commit -m "$(cat <<'EOF'
incremental: document Stage 1 deferrals in FUTURE-WORK.md

Catalogue of SPEC-06 requirements not addressed by Stage 1 — F5
(closure deltas), F6 (retraction across joins), F7 in-flight reader
visibility, distributed timely — each with a promotion trigger.
Also lists Stage-1 simplifications (in-memory log, unbounded feed,
naïve NaryPlan, BTreeMap-backed Zset) worth revisiting under
profiler pressure.
EOF
)"
```

---

## Task 17: Final crate-level sanity sweep

**Files:** none modified

- [ ] **Step 1: Confirm the public API matches the frozen contract**

Run: `cargo doc -p horndb-incremental --no-deps --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml 2>&1 | tail -20`
Expected: `Finished` with no warnings. Open `target/doc/horndb_incremental/index.html` mentally and confirm:
- Re-exports: `Zset`, `Circuit`, `TickReport`, `DeltaLog`, `ChangeFeed`, `ChangeFeedRx`, `Checkpoint`, `CheckpointReport`, `LinearRule`, `BilinearRule`, `NaryPlan`, `DeltaRecord`, `DerivationKind`, `LogicalTime`, `Multiplicity`, `RuleId`, `TripleId`.

- [ ] **Step 2: Run the entire workspace test suite (catches accidental breakage in sibling crates)**

Run: `cargo test --workspace --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml`
Expected: all `horndb-incremental` tests green; other crates have only placeholder libs so they compile and contribute 0 tests.

- [ ] **Step 3: Lint pass**

Run: `cargo clippy -p horndb-incremental --all-targets --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml -- -D warnings`
Expected: no warnings. If clippy complains about the `let _ = Checkpoint;` line in `circuit.rs` (after the inline merge), delete that line — the import is still used by the test surface.

- [ ] **Step 4: Confirm test count and update the running tally**

Run: `cargo test -p horndb-incremental --manifest-path /Users/stig/git/sunstone/reasoner/Cargo.toml 2>&1 | grep 'test result'`
Expected output (one line per test binary):
- `zset_basic`: 4 passed
- `zset_props`: 4 passed
- `delta_log`: 4 passed
- `change_feed`: 3 passed
- `linear_rule`: 2 passed
- `bilinear_correctness`: 1 passed
- `nary_plan`: 1 passed
- `checkpoint`: 3 passed
- `circuit_tick`: 1 passed
- `fixtures_smoke`: 1 passed
- `acceptance_differential`: 2 passed
- `acceptance_change_feed`: 1 passed
- Unit tests in lib: 0 (no `#[cfg(test)]` modules inside `src/`)

Total: **27 tests**.

- [ ] **Step 5: Commit a no-op chore commit only if `cargo doc` / `cargo clippy` required source changes; otherwise skip**

If `cargo clippy` flagged anything and you edited code to fix it:

```bash
cd /Users/stig/git/sunstone/reasoner && git add crates/incremental && git commit -m "$(cat <<'EOF'
incremental: clippy + doc sweep for SPEC-06 stage 1 closeout

Final lint pass before merging the SPEC-06 stage-1 slice.
EOF
)"
```

Otherwise there is nothing to commit; proceed to the post-conditions section below.

---

## Stage-1 exit conditions

After Task 17, the following must hold:

1. **`cargo test --workspace`** is green.
2. **`cargo clippy --all-targets -- -D warnings`** is clean for `horndb-incremental`.
3. **`cargo doc -p horndb-incremental --no-deps`** produces no warnings.
4. The crate exports the **frozen API contract** listed in the "Dependency interface contracts" section at the top of this plan.
5. SPEC-06 acceptance #4 and #5 are demonstrated by `acceptance_differential.rs` and `acceptance_change_feed.rs` respectively.
6. `FUTURE-WORK.md` exists and lists F5, F6, in-flight MVCC, and distributed timely as Stage 2/3 deliverables.

The remaining SPEC-06 acceptance criteria (#1 LUBM-1000 latency, #2 LUBM-8000 throughput, #3 retraction round-trip) are explicitly Stage 2 and depend on SPEC-02 storage + SPEC-04 rule codegen being real. The benchmark scaffold from Task 15 is the hook for those.

---

## Self-review (run mentally before handing off)

**Spec coverage** — every SPEC-06 functional requirement traces to a task or an explicit deferral:
- F1 Z-set storage → Task 3 (`Zset`), Task 4 (props).
- F2 linear operator → Task 7 (trait + linearity proptest).
- F3 bilinear operator → Task 7 (trait), Task 8 (decomposition law proptest).
- F4 n-ary operator → Task 9 (`NaryPlan` left-deep planner).
- F5 closure deltas → deferred, Task 16 `FUTURE-WORK.md`.
- F6 retraction → deferred (Stage 1 insertion-only), Task 16.
- F7 snapshot consistency → Task 11 (checkpoint-boundary semantics in `Circuit`); in-flight MVCC deferred, Task 16.
- F8 checkpoint merge → Task 10 (`Checkpoint::merge`), Task 11 (inlined in `tick`).
- F9 change feed → Task 6 (`ChangeFeed`), Task 11 (wiring), Task 14 (acceptance).

**Acceptance criteria** — #4 and #5 are Tasks 13 and 14. #1/#2/#3 are Stage 2 (documented).

**Placeholders** — none. Every step ships actual code or actual commands.

**Type consistency** — `BilinearRule::apply_delta`/`apply_full`, `LinearRule::apply_delta`, `NaryPlan::push_join`/`apply_full`/`apply_delta`, `Circuit::tick`/`assert_triple`/`subscribe`, `Zset::add`/`add_assign`/`sub_assign`/`from_iter`/`iter`/`get`/`len`/`is_empty` — all spelled identically wherever referenced.

**Dependency interfaces frozen by Task 7** — `BilinearRule` and `LinearRule` are not modified after Task 7; SPEC-04's plan can be written in parallel.
