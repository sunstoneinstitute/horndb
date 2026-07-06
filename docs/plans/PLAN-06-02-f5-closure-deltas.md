---
status: executed
date: 2026-06-01
scope: "SPEC-06 F5 — Closure-Operator Deltas"
---

# SPEC-06 F5 — Closure-Operator Deltas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire SPEC-05's `IncrementalClosureBackend` into the SPEC-06 `Circuit` so inserting transitive-predicate edges emits only the closure *delta*, tagged `DerivationKind::ClosureInferred` on the change feed (insertion-only).

**Architecture:** Add `horndb-closure` as a dependency of `horndb-incremental` (sanctioned by the workspace layering `… → {owlrl, closure} → incremental → …`). Introduce a `ClosureRule` trait in `horndb-incremental` that abstracts "apply an asserted insertion delta, return newly inferred closure triples"; provide a concrete `TransitiveClosureRule` adapter that wraps `IncrementalClosureBackend` for one transitive predicate. `Circuit` gains a `closure_plans` slot and an `add_closure_plan(...)`; on `tick()` it runs each closure plan over the asserted delta, dedups against the combined base (same as the rule path), merges the inferred triples into `derived_base`, and publishes them with `DerivationKind::ClosureInferred`.

**Tech Stack:** Rust 2021, `horndb-incremental` (hand-rolled Z-set core), `horndb-closure` (`IncrementalClosureBackend` / `IncrementalTransitiveClosure`, GraphBLAS-linked), `std::sync::Mutex` for the collecting sink.

---

## Background / key facts (read before starting)

- `TripleId = (u64, u64, u64)` is `(s, p, o)` in dictionary IDs (`crates/incremental/src/types.rs`).
- `DerivationKind::ClosureInferred` already exists (`types.rs`) but is **never produced** today — this plan makes it produced.
- The matrix side already shipped (SPEC-05 #42):
  - `horndb_closure::sink::IncrementalClosureBackend` with
    `insert_transitive_edges(p: PredicateId, new_edges: &[(DictId, DictId)], sink: &dyn TripleSink) -> Result<u64>`
    — retains per-predicate closed state, folds in only new edges, writes **only the delta** triples to the sink.
  - `horndb_closure::sink::TripleSink` — `fn bulk_insert_inferred(&self, triples: &mut dyn Iterator<Item = Triple>) -> Result<u64>`; trait bound `TripleSink: Sync`.
  - `horndb_closure::types::{DictId(pub u64), PredicateId(pub u64), Triple{s: DictId, p: PredicateId, o: DictId}}`.
  - For the **oracle** in the differential test: `horndb_closure::sink::{BackendImpl, ClosureBackend}` — `close_transitive_predicate(p, edges, sink)` recomputes the full closure from scratch (writes inferred edges including the asserted ones).
- The `IncrementalClosureBackend` delta for inserting `(s,o)` **includes the direct edge `(s,o)`** itself when newly added. The Circuit must therefore dedup closure output against the combined base (asserted+derived) exactly like the rule path does, so an asserted edge is not re-emitted as `ClosureInferred`.
- Current `Circuit::tick` (`crates/incremental/src/circuit.rs`): drains the pending log into `asserted_base`, captures `asserted_delta`, runs a fixed-point loop over `self.plans` (rule `NaryPlan`s) emitting `RuleInferred`, dedups via `combined_base.get(triple) == 0`, advances `derived_clock`, publishes to the feed. This plan adds a closure pass after the rule loop.
- Build/test in the worktree reuses the main repo's warm target dir to avoid recompiling rocksdb:
  `export CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target`.
  Building `horndb-incremental` now pulls in `horndb-closure`, which links GraphBLAS — the worktree already has the vendored submodule and shared GraphBLAS build, so this works.

## File Structure

- **Modify** `crates/incremental/Cargo.toml` — add `horndb-closure` dependency (path + workspace-style).
- **Create** `crates/incremental/src/closure_plan.rs` — `ClosureRule` trait, `TransitiveClosureRule` adapter, internal collecting `VecSink`.
- **Modify** `crates/incremental/src/lib.rs` — declare `pub mod closure_plan;` and re-export `ClosureRule`, `TransitiveClosureRule`.
- **Modify** `crates/incremental/src/circuit.rs` — `closure_plans` field, `add_closure_plan`, closure pass in `tick()`.
- **Create** `crates/incremental/tests/closure_deltas.rs` — unit + change-feed tests.
- **Create** `crates/incremental/tests/closure_deltas_differential.rs` — proptest differential vs full recompute.
- **Modify** `crates/incremental/FUTURE-WORK.md` — move F5 from Stage-2 "out" to "delivered (insertion-only)".
- **Modify** `crates/incremental/src/circuit.rs` module doc — drop the "Closure deltas (F5) are not invoked here" line.
- Bookkeeping (closing commit, Task 9): `TASKS.md`, `docs/architecture.md`.

---

### Task 1: Add the `horndb-closure` dependency

**Files:**
- Modify: `crates/incremental/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/incremental/Cargo.toml`, under `[dependencies]`, add a line matching how other intra-workspace crates are referenced. Check an existing crate that depends on another workspace crate (e.g. `grep -rn "horndb-" crates/*/Cargo.toml`) and mirror the exact style. The expected form is a path dependency:

```toml
horndb-closure = { path = "../closure" }
```

- [ ] **Step 2: Verify it resolves and compiles**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo build -p horndb-incremental`
Expected: PASS (clean build; `horndb-closure` compiles as a dependency — GraphBLAS links via the vendored submodule).

- [ ] **Step 3: Commit**

```bash
git add crates/incremental/Cargo.toml Cargo.lock
git commit -m "build(incremental): depend on horndb-closure for SPEC-06 F5"
```

---

### Task 2: `ClosureRule` trait + collecting sink (failing test first)

**Files:**
- Create: `crates/incremental/src/closure_plan.rs`
- Modify: `crates/incremental/src/lib.rs`
- Test: inline `#[cfg(test)]` in `closure_plan.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/incremental/src/closure_plan.rs` with only the test module and a `use super::*;` so it fails to compile (types not yet defined):

```rust
//! SPEC-06 F5 — closure-operator delta plans.
//!
//! A `ClosureRule` consumes the asserted insertion delta for a tick and
//! returns the newly inferred closure triples (insertion-only). The concrete
//! `TransitiveClosureRule` wraps SPEC-05's `IncrementalClosureBackend` for one
//! transitive predicate, mapping `TripleId` ⇄ closure dictionary IDs and
//! collecting the delta triples the backend writes.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zset::Zset;

    /// p = 100. Inserting the chain (1,p,2),(2,p,3) in one delta yields the
    /// transitive edge (1,p,3) plus the two direct edges as inferred output.
    #[test]
    fn transitive_rule_chain_one_delta() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((1, 100, 2), 1);
        delta.add((2, 100, 3), 1);
        let mut got = rule.apply_insert_delta(&delta);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 100, 2), (1, 100, 3), (2, 100, 3)]);
    }

    /// Edges for other predicates are ignored by a rule bound to p=100.
    #[test]
    fn transitive_rule_ignores_other_predicates() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut delta: Zset<crate::types::TripleId> = Zset::new();
        delta.add((1, 100, 2), 1);
        delta.add((1, 999, 2), 1); // different predicate
        let got = rule.apply_insert_delta(&delta);
        assert_eq!(got, vec![(1, 100, 2)]);
    }

    /// State is retained across calls: the second delta sees the first.
    #[test]
    fn transitive_rule_retains_state_across_deltas() {
        let mut rule = TransitiveClosureRule::new(100);
        let mut d1: Zset<crate::types::TripleId> = Zset::new();
        d1.add((1, 100, 2), 1);
        let _ = rule.apply_insert_delta(&d1);

        let mut d2: Zset<crate::types::TripleId> = Zset::new();
        d2.add((2, 100, 3), 1);
        let mut got = rule.apply_insert_delta(&d2);
        got.sort_unstable();
        // Only the *new* edges: (2,3) direct and (1,3) transitive. (1,2) was
        // already emitted in the first delta and is not re-emitted.
        assert_eq!(got, vec![(1, 100, 3), (2, 100, 3)]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails (compile error)**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental --lib closure_plan 2>&1 | tail -20`
Expected: FAIL — `cannot find ... TransitiveClosureRule` / unresolved imports.

- [ ] **Step 3: Implement the trait, adapter, and sink**

Prepend to `crates/incremental/src/closure_plan.rs` (above the test module):

```rust
use std::sync::Mutex;

use horndb_closure::sink::{IncrementalClosureBackend, TripleSink};
use horndb_closure::types::{DictId, PredicateId, Triple};

use crate::types::TripleId;
use crate::zset::Zset;

/// A closure operator maintained incrementally under insertions (SPEC-06 F5).
///
/// Given the asserted insertion delta for a tick, returns the newly inferred
/// closure triples. Implementations retain their own closed state across
/// calls, so each call only needs this tick's new edges. Insertion-only:
/// negative multiplicities are ignored (retraction is F6, deferred).
pub trait ClosureRule {
    fn apply_insert_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId>;
}

/// Collects the delta triples written by `IncrementalClosureBackend`.
///
/// `TripleSink` requires `Sync`, so we use a `Mutex` rather than a `RefCell`.
/// The sink is short-lived: created per `apply_insert_delta` call, drained
/// immediately after.
#[derive(Default)]
struct VecSink {
    collected: Mutex<Vec<Triple>>,
}

impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> anyhow::Result<u64> {
        let mut guard = self.collected.lock().expect("VecSink lock poisoned");
        let before = guard.len();
        guard.extend(triples);
        Ok((guard.len() - before) as u64)
    }
}

/// Incremental transitive closure for a single predicate `p`, wrapping
/// SPEC-05's `IncrementalClosureBackend`.
///
/// `p` is the predicate component of the `TripleId`s this rule handles; only
/// asserted-delta triples whose middle component equals `p` and whose
/// multiplicity is positive contribute edges. The backend emits only the
/// newly inferred edges (including a freshly inserted direct edge), so output
/// is already deduplicated against the rule's own retained closure.
pub struct TransitiveClosureRule {
    predicate: u64,
    backend: IncrementalClosureBackend,
}

impl TransitiveClosureRule {
    pub fn new(predicate: u64) -> Self {
        Self {
            predicate,
            backend: IncrementalClosureBackend::new(),
        }
    }
}

impl ClosureRule for TransitiveClosureRule {
    fn apply_insert_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId> {
        // Collect positive-multiplicity edges for this predicate.
        let edges: Vec<(DictId, DictId)> = asserted_delta
            .iter()
            .filter(|((_, p, _), mult)| *p == self.predicate && *mult > 0)
            .map(|((s, _, o), _)| (DictId(*s), DictId(*o)))
            .collect();
        if edges.is_empty() {
            return Vec::new();
        }
        let sink = VecSink::default();
        let pid = PredicateId(self.predicate);
        // The in-memory VecSink never errors; surface a panic if the backend
        // itself does (GraphBLAS-level failure is not recoverable here).
        self.backend
            .insert_transitive_edges(pid, &edges, &sink)
            .expect("incremental closure insert failed");
        let collected = sink
            .collected
            .into_inner()
            .expect("VecSink lock poisoned");
        collected
            .into_iter()
            .map(|t| (t.s.0, t.p.0, t.o.0))
            .collect()
    }
}
```

Add to `crates/incremental/src/lib.rs` the module declaration (alongside the others) and re-export:

```rust
pub mod closure_plan;
```
```rust
pub use closure_plan::{ClosureRule, TransitiveClosureRule};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental --lib closure_plan 2>&1 | tail -20`
Expected: PASS — all three `closure_plan::tests::*` green.

- [ ] **Step 5: Commit**

```bash
git add crates/incremental/src/closure_plan.rs crates/incremental/src/lib.rs
git commit -m "feat(incremental): ClosureRule trait + TransitiveClosureRule adapter (SPEC-06 F5)"
```

---

### Task 3: `Circuit::add_closure_plan` + tick integration (failing test first)

**Files:**
- Test: `crates/incremental/tests/closure_deltas.rs` (create)
- Modify: `crates/incremental/src/circuit.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/incremental/tests/closure_deltas.rs`:

```rust
//! SPEC-06 F5 — closure-operator deltas through the Circuit.

use horndb_incremental::{Circuit, DerivationKind, TransitiveClosureRule};

const P: u64 = 100;

/// Inserting a chain across two ticks yields the transitive edge, emitted
/// once as a ClosureInferred derived triple.
#[test]
fn chain_closure_across_ticks() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    c.assert_triple((1, P, 2));
    c.tick();
    c.assert_triple((2, P, 3));
    c.tick();

    // asserted base has the two direct edges.
    assert_eq!(c.asserted_base().get(&(1, P, 2)), 1);
    assert_eq!(c.asserted_base().get(&(2, P, 3)), 1);
    // derived base has the transitive edge, and NOT the direct edges
    // (those are asserted, deduped out of the closure output).
    assert_eq!(c.derived_base().get(&(1, P, 3)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 2)), 0);
    assert_eq!(c.derived_base().get(&(2, P, 3)), 0);
}

/// A single tick that inserts a full chain closes it in one pass.
#[test]
fn chain_closure_one_tick() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));
    c.assert_triple((1, P, 2));
    c.assert_triple((2, P, 3));
    c.assert_triple((3, P, 4));
    c.tick();
    assert_eq!(c.derived_base().get(&(1, P, 3)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 4)), 1);
    assert_eq!(c.derived_base().get(&(2, P, 4)), 1);
    // direct edges stay asserted-only.
    assert_eq!(c.derived_base().get(&(1, P, 2)), 0);
}

/// The change feed receives each closure-inferred triple once, tagged
/// ClosureInferred (F9), with no duplicates.
#[test]
fn change_feed_tags_closure_inferred() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));
    let rx = c.subscribe();

    c.assert_triple((1, P, 2));
    c.assert_triple((2, P, 3));
    c.tick();

    let mut asserted = Vec::new();
    let mut closure = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        match rec.kind {
            DerivationKind::Asserted => asserted.push(rec.triple),
            DerivationKind::ClosureInferred => closure.push(rec.triple),
            DerivationKind::RuleInferred(_) => panic!("no rule plans registered"),
        }
    }
    asserted.sort_unstable();
    closure.sort_unstable();
    assert_eq!(asserted, vec![(1, P, 2), (2, P, 3)]);
    assert_eq!(closure, vec![(1, P, 3)]);
}
```

- [ ] **Step 2: Run test to verify it fails (compile error)**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental --test closure_deltas 2>&1 | tail -20`
Expected: FAIL — `no method named add_closure_plan`.

- [ ] **Step 3: Implement the closure pass in `Circuit`**

In `crates/incremental/src/circuit.rs`:

Add the import near the top (with the other `use crate::` lines):

```rust
use crate::closure_plan::ClosureRule;
```

Add a field to the `Circuit` struct (after `plans`):

```rust
    closure_plans: Vec<Box<dyn ClosureRule>>,
```

Initialise it in `Circuit::new()` (after `plans: Vec::new(),`):

```rust
            closure_plans: Vec::new(),
```

Add the registration method (after `add_plan`):

```rust
    /// Register a closure operator (SPEC-06 F5). On each tick its
    /// `apply_insert_delta` runs over the asserted insertion delta and the
    /// newly inferred triples are merged into `derived_base`, published as
    /// `DerivationKind::ClosureInferred`.
    pub fn add_closure_plan(&mut self, rule: Box<dyn ClosureRule>) {
        self.closure_plans.push(rule);
    }
```

In `tick()`, after the rule fixed-point `for` loop closes (just before constructing `TickReport`), add the closure pass. Note `combined_base` already holds asserted+derived including this tick's rule output, and `asserted_delta` was moved into `round_delta`; capture the asserted delta separately so it survives the loop. To do that, change the line that builds `round_delta`:

Replace:
```rust
        let mut round_delta = asserted_delta;
```
with:
```rust
        let asserted_delta_for_closure = asserted_delta.clone();
        let mut round_delta = asserted_delta;
```

Then append after the `for _ in 0..MAX_ROUNDS` loop:

```rust
        // Closure pass (SPEC-06 F5): run each closure operator over the
        // asserted insertion delta. Newly inferred triples not already present
        // in the combined base are merged into derived_base and published as
        // ClosureInferred. Insertion-only; closure↔rule cross-feedback within
        // a tick is a Stage-2 concern (see FUTURE-WORK.md).
        for rule in &mut self.closure_plans {
            let inferred = rule.apply_insert_delta(&asserted_delta_for_closure);
            for triple in inferred {
                if combined_base.get(&triple) != 0 {
                    continue;
                }
                self.derived_base.add(triple, 1);
                combined_base.add(triple, 1);
                let t = self.derived_clock;
                self.derived_clock = self
                    .derived_clock
                    .checked_add(1)
                    .expect("derived-clock overflow");
                self.feed
                    .publish(triple, 1, t, DerivationKind::ClosureInferred);
                derived_merged += 1;
            }
        }
```

(`derived_merged` is already declared `let mut` earlier in `tick`; the closure pass increments the same counter so `TickReport.derived_merged` covers closure output too.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental --test closure_deltas 2>&1 | tail -20`
Expected: PASS — all three tests green.

- [ ] **Step 5: Run the full incremental test suite (no regressions)**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental 2>&1 | tail -25`
Expected: PASS — existing `circuit_tick`, `change_feed`, `acceptance_*`, etc. all still green.

- [ ] **Step 6: Commit**

```bash
git add crates/incremental/src/circuit.rs crates/incremental/tests/closure_deltas.rs
git commit -m "feat(incremental): Circuit closure pass emits ClosureInferred deltas (SPEC-06 F5)"
```

---

### Task 4: Differential test vs full recompute (proptest)

**Files:**
- Test: `crates/incremental/tests/closure_deltas_differential.rs` (create)

- [ ] **Step 1: Confirm `proptest` is available as a dev-dependency**

Run: `grep -n "proptest" crates/incremental/Cargo.toml`
Expected: a `proptest` line under `[dev-dependencies]` (the crate already uses it for `acceptance_differential.rs`). If absent, add `proptest.workspace = true` under `[dev-dependencies]` and include that file in the Task 4 commit.

- [ ] **Step 2: Write the differential test**

Create `crates/incremental/tests/closure_deltas_differential.rs`:

```rust
//! SPEC-06 F5 differential test: the Circuit closure path produces the same
//! transitive closure as a full from-scratch recompute (SPEC-05 `BackendImpl`),
//! for an arbitrary sequence of edge insertions split across arbitrary tick
//! boundaries. Insertion-only (SPEC-06 acceptance #4 shape, closure subset).

use std::collections::BTreeSet;
use std::sync::Mutex;

use horndb_closure::sink::{BackendImpl, ClosureBackend, TripleSink};
use horndb_closure::types::{DictId, PredicateId, Triple};
use horndb_incremental::{Circuit, TransitiveClosureRule};
use proptest::prelude::*;

const P: u64 = 7;

/// Collecting sink for the oracle.
#[derive(Default)]
struct VecSink {
    collected: Mutex<Vec<Triple>>,
}
impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> anyhow::Result<u64> {
        let mut g = self.collected.lock().unwrap();
        let before = g.len();
        g.extend(triples);
        Ok((g.len() - before) as u64)
    }
}

/// Full closure of `edges` under predicate P, as a set of (s,o) pairs.
fn oracle_closure(edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    if edges.is_empty() {
        return BTreeSet::new();
    }
    let mut backend = BackendImpl::default();
    let sink = VecSink::default();
    let dict_edges: Vec<(DictId, DictId)> =
        edges.iter().map(|&(s, o)| (DictId(s), DictId(o))).collect();
    backend
        .close_transitive_predicate(PredicateId(P), &dict_edges, &sink)
        .unwrap();
    sink.collected
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|t| (t.s.0, t.o.0))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For a random edge list and random tick split points, the Circuit's
    /// (asserted ∪ derived) support for predicate P equals the full closure.
    #[test]
    fn circuit_closure_matches_full_recompute(
        edges in prop::collection::vec((0u64..8, 0u64..8), 0..24),
        // tick after every k-th edge; k in 1..=4 controls batching.
        tick_every in 1usize..=4,
    ) {
        let mut c = Circuit::new();
        c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

        for (i, &(s, o)) in edges.iter().enumerate() {
            c.assert_triple((s, P, o));
            if (i + 1) % tick_every == 0 {
                c.tick();
            }
        }
        c.tick(); // flush any remaining

        // Circuit's materialized support for predicate P.
        let mut got: BTreeSet<(u64, u64)> = BTreeSet::new();
        for ((s, p, o), m) in c.asserted_base().iter() {
            if *p == P && m > 0 {
                got.insert((*s, *o));
            }
        }
        for ((s, p, o), m) in c.derived_base().iter() {
            if *p == P && m > 0 {
                got.insert((*s, *o));
            }
        }

        let want = oracle_closure(&edges);
        prop_assert_eq!(got, want);
    }
}
```

- [ ] **Step 3: Run the differential test**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test -p horndb-incremental --test closure_deltas_differential 2>&1 | tail -25`
Expected: PASS — 256 cases, `circuit_closure_matches_full_recompute` green. If it fails, proptest writes a regression file; investigate the minimal failing case (do not delete the regression file).

- [ ] **Step 4: Commit**

```bash
git add crates/incremental/tests/closure_deltas_differential.rs crates/incremental/Cargo.toml Cargo.lock
git commit -m "test(incremental): differential closure-delta vs full recompute (SPEC-06 F5)"
```

---

### Task 5: Update crate docs (FUTURE-WORK + module doc)

**Files:**
- Modify: `crates/incremental/FUTURE-WORK.md`
- Modify: `crates/incremental/src/circuit.rs` (module doc only)

- [ ] **Step 1: Update FUTURE-WORK.md**

In `crates/incremental/FUTURE-WORK.md`, replace the `### F5 — Closure-operator deltas (SPEC-05 integration)` section's body with a "delivered (insertion-only)" note. Replace:

```markdown
### F5 — Closure-operator deltas (SPEC-05 integration)
- **Now**: not invoked. The `Circuit` has no `ClosurePlan` slot.
- **Stage 2**: add `add_closure_plan(...)` and a `ClosureRule` trait
  that wraps a GraphBLAS matrix-power step. SPEC-05 owns the matrix
  side; SPEC-06 owns the delta integration.
```

with:

```markdown
### F5 — Closure-operator deltas (SPEC-05 integration) — DELIVERED (insertion-only)
- **Done (2026-06-01, #44)**: `Circuit::add_closure_plan(Box<dyn ClosureRule>)`
  registers a closure operator. `TransitiveClosureRule`
  (`crates/incremental/src/closure_plan.rs`) wraps SPEC-05's
  `IncrementalClosureBackend`; on each tick it folds the asserted insertion
  delta into the retained per-predicate closure and emits only the newly
  inferred triples, published as `DerivationKind::ClosureInferred`. Differential
  test (`tests/closure_deltas_differential.rs`) pins it against the full
  `BackendImpl` recompute.
- **Still Stage 2**: retraction / negative-multiplicity deltas through the
  closure (needs F6 below + the deletion half of SPEC-05's incremental
  closure); closure↔rule cross-feedback *within* a single tick (closure output
  feeding rule bodies and vice versa); non-transitive closure shapes.
```

- [ ] **Step 2: Update the `circuit.rs` module doc**

In `crates/incremental/src/circuit.rs`, in the `//! Stage 1 simplifications:` block, replace the line:

```rust
//! - Closure deltas (F5) are not invoked here; SPEC-05 stage 2 wires
//!   in via a `add_closure_plan` extension.
```

with:

```rust
//! - Closure deltas (F5) run after the rule fixed-point via
//!   `add_closure_plan` / `ClosureRule` (insertion-only). Closure↔rule
//!   cross-feedback within one tick remains a Stage-2 concern.
```

- [ ] **Step 3: Verify the crate still builds and the doc references are accurate**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo build -p horndb-incremental 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/incremental/FUTURE-WORK.md crates/incremental/src/circuit.rs
git commit -m "docs(incremental): mark SPEC-06 F5 closure deltas delivered (insertion-only)"
```

---

### Task 6: Full verification gate (fmt, clippy, workspace tests)

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Clippy (the CI gate)**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30`
Expected: PASS — no warnings. Fix any clippy findings in the new code (likely candidates: `needless_borrow`, redundant closures in the edge filter). If you fix anything, re-run and commit with `chore(incremental): clippy fixes for SPEC-06 F5`.

- [ ] **Step 3: Full workspace test run**

Run: `CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target cargo test --workspace 2>&1 | tail -40`
Expected: PASS — all crates green, including the new `closure_deltas` + `closure_deltas_differential` tests. (No SPARQL server feature touched, so the `--features server` run is not required for this task.)

- [ ] **Step 4: Commit any formatting/clippy fixups**

```bash
git add -A
git commit -m "chore(incremental): fmt/clippy for SPEC-06 F5" || echo "nothing to commit"
```

---

### Task 7: Bookkeeping (closing commit — merge-gated)

> Per `/next-task` Phase 7: this is an **epic increment**, so the parent task
> #6 stays `[v]`. Mark only the #44 increment delivered in the breakdown note;
> do NOT flip the parent to `[x]`. Do not close any issue here — closure is
> merge-gated (Phase 11). The `Closes #44` lives in the PR body.

**Files:**
- Modify: `TASKS.md`
- Modify: `docs/architecture.md`

- [ ] **Step 1: Update the SPEC-06 breakdown note in `TASKS.md`**

In `TASKS.md`, in the SPEC-06 body's "Epic breakdown" note, mark #44 delivered. Change the `#44` bullet from describing it as "**first increment**" pending to "**first increment, delivered 2026-06-01**", mirroring the style used for #42/#34 in the sibling epics. Keep the parent line `[v]`. Example edit — change:

```
    [#44](https://github.com/sunstoneinstitute/horndb/issues/44) **F5
    closure-operator deltas** (SPEC-05 integration) — **first increment**:
```
to:
```
    [#44](https://github.com/sunstoneinstitute/horndb/issues/44) **F5
    closure-operator deltas** (SPEC-05 integration) — **first increment,
    delivered 2026-06-01**:
```

- [ ] **Step 2: Update `docs/architecture.md` Status for SPEC-06**

Find the SPEC-06 row/section in `docs/architecture.md` (`grep -n "SPEC-06\|incremental" docs/architecture.md`). Update its Status note so closure-delta (F5) insertion-path maintenance reads as **implemented**, while retraction (F6) and MVCC (F7) stay **planned**. Match the surrounding wording style; do not invent a new Status taxonomy. Example: append to the SPEC-06 entry "F5 closure-operator deltas (insertion-only) implemented (#44); F6 retraction and F7 MVCC planned."

- [ ] **Step 3: Verify the docs are internally consistent**

Run: `grep -n "#44\|SPEC-06" TASKS.md docs/architecture.md`
Expected: the #44 increment shows delivered in `TASKS.md`; `docs/architecture.md` shows F5 implemented. Parent #6 task line is still `[v]`.

- [ ] **Step 4: Commit**

```bash
git add TASKS.md docs/architecture.md
git commit -m "docs(tasks): SPEC-06 F5 closure deltas delivered (#44); architecture status"
```

---

## Self-Review notes

- **Spec coverage (SPEC-06 F5):** "insertion of `?a p ?b` triggers a recomputation of the reachable-set delta in `M_p*` rather than a full re-closure" → Tasks 2–3 (TransitiveClosureRule + Circuit pass); F9 `derivation_kind = closure-inferred` → Task 3 change-feed test; acceptance #4 (differential vs full rematerialization) → Task 4. Retraction (F6) and MVCC (F7) are explicitly out of this increment (separate sub-issues #45/#46).
- **Type consistency:** `apply_insert_delta(&Zset<TripleId>) -> Vec<TripleId>` is used identically in `closure_plan.rs`, the unit tests, and the Circuit pass. `TripleId = (u64,u64,u64)`; closure `Triple{s,p,o}` maps via `(t.s.0, t.p.0, t.o.0)`. `TransitiveClosureRule::new(u64)` used consistently in all tests.
- **Dedup semantics:** the Circuit closure pass dedups against `combined_base` exactly like the rule pass, so asserted edges re-emitted in the closure delta are not double-published; only genuinely new transitive edges become `ClosureInferred`. The differential test compares the *union* of asserted+derived, so this dedup does not lose edges.
- **Placeholder scan:** no TBD/TODO; every code step shows full code; the only "find the right line" steps (Task 1 dep style, Task 7 architecture row) are inherently context-dependent and include the exact grep to locate them.
