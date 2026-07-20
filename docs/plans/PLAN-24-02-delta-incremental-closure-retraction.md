---
status: executed
date: 2026-07-20
scope: "SPEC-24 S2 — output-sensitive closure deletion (support-counting decremental transitive closure, recompute fallback retained) + exact warm-store seeded retraction (seed_base_edges)"
---

# SPEC-24 S2 — Delta-Incremental Closure Retraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `IncrementalTransitiveClosure::delete_edge` output-sensitive — a deletion costs O(closure delta + frontier inspected), not O(store) — and add an exact warm-store base seed (`seed_base_edges`) that closes the documented under-withdraw hole of the closed-extent seed. Tracking issue: [#211](https://github.com/sunstoneinstitute/horndb/issues/211).

**Architecture:** Keep the existing closed adjacency (`fwd`/`bwd`) as the membership source of truth and the existing outer-product **insert** path unchanged. Replace the deletion algorithm: today it recomputes full base reachability (`base_reach`) from every affected source — a full BFS per source, so a delete that withdraws nothing on a large store still walks the whole reachable subgraph. The new algorithm is a **support-counting backward worklist** (a decremental transitive-closure sweep): a closed pair `(x, y)` is supported iff `x` has at least one base out-edge `z` with `z == y` or `z` still reaches `y` (`closed(z, y)`). "Support" is computed live in O(out-degree of `x`) by scanning `x`'s base neighbours — no stored counters, so **insert needs no change**. Deleting base edge `(s, o)` re-checks only the pairs `(s, y)` that could have used `o` as a first hop (`y` reachable from `o`), and cascades backward along base predecessor edges (new `bwd_base` adjacency) only through pairs that actually lose all support. The old full-recompute algorithm is retained as a per-instance fallback strategy (spec requirement: keep a per-predicate recompute fallback), and the differential proptests run **both** strategies against the GraphBLAS oracle.

**Tech Stack:** Rust 1.90, `proptest`, `criterion`, existing `horndb-closure` / `horndb-incremental`. Work is in `crates/closure/` (algorithm + API) and `crates/incremental/` (rule wrapper) plus docs.

---

## Design (read this before any task)

### Why the old deletion path is not output-sensitive

`IncrementalTransitiveClosure::delete_edge` (in
`crates/closure/src/closure/incremental.rs`) removes `(s, o)` from `base`, then
for each affected source `x` in `closed-bwd[s] ∪ {s}` it calls `base_reach(x)`
— a **full forward BFS over the entire base subgraph reachable from `x`** — and
withdraws candidate pairs no longer reachable. Even when the deletion withdraws
nothing (an alternate path still supports every pair), every source pays a full
BFS. Cost scales with the store, not with what changed.

### The support-counting characterization

For a strict transitive closure with base edge set `B` and closed set
`C = transitive_closure(B)`, define for a closed pair `(x, y)`:

```
support(x, y) = #{ z : base(x, z)  AND  (z == y OR closed(z, y)) }
```

i.e. the number of `x`'s **base out-edges** whose target either *is* `y` or
still reaches `y` in the closure. Two facts make this the right handle:

1. **`closed(x, y)  ⟺  support(x, y) ≥ 1`.** If `x` reaches `y` there is a
   base path whose first hop `z` witnesses it (`z == y` or `z` reaches `y`);
   conversely any witnessing first hop proves `x` reaches `y`.
2. **`support` is cheap to evaluate on the fly.** Scanning `x`'s base
   out-neighbours and testing `closed(z, y)` (an O(1) `fwd[z]` membership
   check) computes it in O(out-degree of `x`). No stored counters — so the
   **insert path does not change at all**.

### Output-sensitive deletion algorithm

Delete base edge `(s, o)`:

1. Remove `(s, o)` from `base` and from `bwd_base` (new backward base
   adjacency). If it was not an asserted edge, return an empty outcome.
2. **Seed** a worklist with every currently-closed pair `(s, y)` where
   `y ∈ {o} ∪ closed-fwd[o]` — exactly the pairs that could have used the
   deleted first hop `o`.
3. **Process** the worklist to a fixpoint. Pop `(x, y)`; skip if already
   withdrawn. Compute `support(x, y)` live:
   - `≥ 1`: the pair keeps an alternate witness — it survives; do not cascade.
   - `0`: withdraw it (`drop_closed(x, y)`, record in `withdrawn`), then for
     each base predecessor `w ∈ bwd_base[x]` enqueue `(w, y)` — `w` may have
     used the now-dead `x` as its first-hop witness for column `y`.
4. **Survivor:** the deleted edge `(s, o)` is a survivor iff it is still closed
   at the end (`fwd[s]` contains `o` — another base path `s ⇒ o` remains). The
   SPEC-06 layer promotes survivors to materialized derived rows (BUG P1).

**Why it is correct.** A temporarily-alive witness `(z, y)` that should die is
re-examined: when `(z, y)` is later withdrawn, every `x` with `base(x, z)`
(so `x ∈ bwd_base[z]`) is enqueued and rechecked, so the over-count
self-corrects — the standard decremental worklist fixpoint. Completeness: any
pair `(x, y)` that must be withdrawn had all its paths pass through `(s, o)`;
following base first-hops forward along such a path reaches `s`, and its
column-`y` pair `(s, y)` is seeded (because `y` is reachable from `o`). The
backward cascade along `bwd_base` then reaches `(x, y)`. Cycles and self-loops
`(x, x)` are handled by the same uniform definition.

**Why it is output-sensitive.** Work = O(|frontier `F`| ) for the seed
(`F = {o} ∪ closed-fwd[o]`, the pairs that genuinely could have lost the
deleted hop) + Σ over each processed pair of O(base out-degree) for its support
check, and a pair is only processed when a witness of it died. Total is
proportional to the withdrawn pairs plus the inspected frontier — independent
of the store size. This matches SPEC-24 acceptance #2 ("deletion cost scales
with the closure delta, not the store").

### The recompute fallback (spec requirement)

Keep the current algorithm verbatim as `delete_edge_recompute` and select
between the two with a per-instance `DeleteStrategy` enum (default
`SupportCounting`). `base_reach` stays (it backs the fallback). The differential
proptests run **both** strategies and assert identical results against the
GraphBLAS bulk closure, so the recompute oracle never leaves the tree and any
future density where counting loses can flip a single flag.

### Exact warm-store seeded retraction (`seed_base_edges`)

`seed_transitive_closure` / `TransitiveClosureRule::seed_closed_edges` seed from
an already-**closed** extent whose true asserted base is unknown, so `base` is
seeded with the closed extent as a conservative stand-in: retracting a *seeded*
edge is sound but may under-withdraw (redundant transitive edges keep a pair
reachable). `IncrementalTransitiveClosure::from_base_edges` already retains the
**true** base and re-closes it, giving exact retraction. This work exposes that
path end-to-end:

- `IncrementalClosureBackend::seed_base_edges(p, base_edges)` — build the
  predicate state via `from_base_edges` (retains the true base; retraction is
  exact). One-time reclosure cost at seed; documented at the API. The
  closed-extent `seed_transitive_closure` stays for callers that genuinely have
  only the closed set.
- `TransitiveClosureRule::seed_base_edges(base_edges)` — the SPEC-06 wrapper.

### Instrumenting output-sensitivity for a deterministic gate

`delete_edge` accumulates a probe count (`support` evaluations + seeded pairs)
into a `last_delete_probes: usize` field, exposed via
`last_delete_probes()`. A wall-clock criterion bench is env-sensitive; the probe
counter gives a deterministic, CI-friendly assertion that a small-delta delete
on a growing store inspects a bounded amount of work.

### Files

- **Modify** `crates/closure/src/closure/incremental.rs` — add `bwd_base` field
  + maintenance, `DeleteStrategy` enum, support-counting `delete_edge`, retain
  old logic as `delete_edge_recompute`, `last_delete_probes` counter, unit
  tests.
- **Modify** `crates/closure/src/sink.rs` — add `IncrementalClosureBackend::seed_base_edges`; thread `DeleteStrategy` selection; docs.
- **Modify** `crates/incremental/src/closure_plan.rs` — add
  `TransitiveClosureRule::seed_base_edges`; unit test.
- **Create** `crates/closure/tests/retraction_strategies_differential.rs` —
  proptest: both strategies == GraphBLAS oracle, identical withdrawn sets.
- **Create** `crates/closure/tests/retraction_output_sensitive.rs` —
  probe-count scaling test (small delta, growing store) + seeded-base exact test.
- **Create** `crates/closure/benches/closure_retraction.rs` + `[[bench]]` entry
  in `crates/closure/Cargo.toml` — support-counting vs recompute A/B.
- **Docs** — this plan; `docs/architecture.md` status; `crates/closure/INTEGRATION-NOTES.md`; `docs/benchmarks.md` row; `docs/specs/SPEC-24-incremental-stage2.md` S2 status note; `docs/index.md`/plan index if needed.

---

## Task 1: Add `bwd_base` backward base adjacency

Maintain a backward view of the asserted base so deletion can cascade to base
predecessors in O(in-degree). This is pure bookkeeping — no behaviour change yet
(the current `delete_edge` still uses `base_reach`).

**Files:**
- Modify: `crates/closure/src/closure/incremental.rs`
- Test: same file (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn bwd_base_tracks_base_predecessors() {
    let mut c = IncrementalTransitiveClosure::from_base_edges([(1, 2), (3, 2)]);
    // Both 1 and 3 are base predecessors of 2.
    let mut preds = c.base_predecessors(2);
    preds.sort_unstable();
    assert_eq!(preds, vec![1, 3]);
    // Deleting (1,2) removes 1 as a predecessor of 2; 3 remains.
    c.delete_edge(1, 2);
    assert_eq!(c.base_predecessors(2), vec![3]);
    // Reinserting restores it.
    c.insert_edge(1, 2);
    let mut preds = c.base_predecessors(2);
    preds.sort_unstable();
    assert_eq!(preds, vec![1, 3]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-closure --lib bwd_base_tracks_base_predecessors`
Expected: FAIL — `no method named base_predecessors`.

- [ ] **Step 3: Implement `bwd_base` field + maintenance**

In the struct add the field (next to `base`):

```rust
    /// Backward adjacency of the asserted base (`bwd_base[o]` = { s : base(s, o) }).
    /// Maintained in lockstep with `base`; lets retraction cascade to a node's
    /// base predecessors in O(in-degree) without scanning the whole base.
    bwd_base: FxHashMap<u64, FxHashSet<u64>>,
```

In `insert_edge_tracked`, right after `let base_was_new = self.base.entry(s).or_default().insert(o);` add:

```rust
        if base_was_new {
            self.bwd_base.entry(o).or_default().insert(s);
        }
```

In `from_closed_edges`, where it seeds `base`, mirror into `bwd_base`:

```rust
            // Seed the same edge into `base`/`bwd_base` as a conservative stand-in
            c.base.entry(s).or_default().insert(o);
            c.bwd_base.entry(o).or_default().insert(s);
```

In `rollback_base_edges`, after removing from `base`, also remove from `bwd_base`:

```rust
            if let Some(set) = self.bwd_base.get_mut(&o) {
                set.remove(&s);
                if set.is_empty() {
                    self.bwd_base.remove(&o);
                }
            }
```

In `delete_edge`, where it removes `(s, o)` from `base` (the `was_base` block),
also remove from `bwd_base`:

```rust
        if was_base {
            if let Some(set) = self.bwd_base.get_mut(&o) {
                set.remove(&s);
                if set.is_empty() {
                    self.bwd_base.remove(&o);
                }
            }
        }
```

(Place this right after the existing `if self.base.get(&s)...` empties-cleanup so
it runs only when the edge was actually a base edge — guard with the `was_base`
value already computed.)

Add the accessor:

```rust
    /// Base predecessors of `node` (`{ s : base(s, node) }`), unordered.
    /// Test/among-internal helper mirroring `base_edges`.
    pub fn base_predecessors(&self, node: u64) -> Vec<u64> {
        self.bwd_base
            .get(&node)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-closure --lib bwd_base_tracks_base_predecessors`
Expected: PASS.

- [ ] **Step 5: Run the existing retraction differential to prove no regression**

Run: `cargo test -p horndb-closure --test incremental_retraction`
Expected: PASS (the `base_edges` invariant still holds; `bwd_base` is additive).

- [ ] **Step 6: Commit**

```bash
git add crates/closure/src/closure/incremental.rs
git commit -m "closure: maintain backward base adjacency (bwd_base) for output-sensitive deletion (#211)"
```

---

## Task 2: Support-counting deletion + recompute fallback

Introduce the `DeleteStrategy` enum, keep the current algorithm as
`delete_edge_recompute`, and implement the output-sensitive
`delete_edge_support_counting`. `delete_edge` dispatches on the instance's
strategy (default `SupportCounting`). Add the `last_delete_probes` counter.

**Files:**
- Modify: `crates/closure/src/closure/incremental.rs`
- Test: same file

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn support_counting_matches_recompute_on_diamond() {
    // Diamond: 1->2, 1->3, 2->4, 3->4. Deleting (2,4) must NOT withdraw (1,4)
    // (still supported via 1->3->4); only the direct (2,4) is withdrawn.
    // Assert both strategies agree.
    for strat in [DeleteStrategy::SupportCounting, DeleteStrategy::Recompute] {
        let mut c = IncrementalTransitiveClosure::from_base_edges([
            (1, 2), (1, 3), (2, 4), (3, 4),
        ]);
        c.set_delete_strategy(strat);
        let out = c.delete_edge(2, 4);
        assert_eq!(out.withdrawn, vec![(2, 4)], "strategy {:?}", strat);
        assert!(out.survived.is_empty(), "strategy {:?}", strat);
        assert_eq!(
            edge_set(&c),
            [(1, 2), (1, 3), (1, 4), (2, 4).0.eq(&2).then_some((1,4)).map_or((3, 4),|p|p)] // placeholder guard, replaced below
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "strategy {:?}", strat
        );
    }
}
```

Replace the fragile `assert_eq!(edge_set...)` above with the explicit expected
set (write it out — no cleverness):

```rust
        let expected: std::collections::BTreeSet<(u64, u64)> =
            [(1, 2), (1, 3), (1, 4), (3, 4)].into_iter().collect();
        assert_eq!(edge_set(&c), expected, "strategy {:?}", strat);
```

So the final test body is:

```rust
#[test]
fn support_counting_matches_recompute_on_diamond() {
    for strat in [DeleteStrategy::SupportCounting, DeleteStrategy::Recompute] {
        let mut c = IncrementalTransitiveClosure::from_base_edges([
            (1, 2), (1, 3), (2, 4), (3, 4),
        ]);
        c.set_delete_strategy(strat);
        let out = c.delete_edge(2, 4);
        assert_eq!(out.withdrawn, vec![(2, 4)], "strategy {:?}", strat);
        assert!(out.survived.is_empty(), "strategy {:?}", strat);
        let expected: std::collections::BTreeSet<(u64, u64)> =
            [(1, 2), (1, 3), (1, 4), (3, 4)].into_iter().collect();
        assert_eq!(edge_set(&c), expected, "strategy {:?}", strat);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-closure --lib support_counting_matches_recompute_on_diamond`
Expected: FAIL — `DeleteStrategy` / `set_delete_strategy` not found.

- [ ] **Step 3: Add the enum, fields, and dispatch**

Above the struct:

```rust
/// Which decremental algorithm `delete_edge` uses.
///
/// `SupportCounting` (default) is output-sensitive: a deletion costs
/// O(closure delta + inspected frontier). `Recompute` is the original
/// affected-region full-reachability recompute, retained per SPEC-24 S2 as a
/// per-predicate fallback and as the differential-test oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteStrategy {
    SupportCounting,
    Recompute,
}

impl Default for DeleteStrategy {
    fn default() -> Self {
        DeleteStrategy::SupportCounting
    }
}
```

Add fields to the struct:

```rust
    strategy: DeleteStrategy,
    /// Pairs inspected by the most recent `delete_edge` (seeded + support
    /// evaluations). Deterministic proxy for deletion cost; drives the
    /// output-sensitivity test. Reset at the start of each `delete_edge`.
    last_delete_probes: usize,
```

Add accessors:

```rust
    /// Select the decremental algorithm (default `SupportCounting`).
    pub fn set_delete_strategy(&mut self, strategy: DeleteStrategy) {
        self.strategy = strategy;
    }

    /// Pairs inspected by the most recent `delete_edge` call.
    pub fn last_delete_probes(&self) -> usize {
        self.last_delete_probes
    }
```

- [ ] **Step 4: Rename the existing algorithm to `delete_edge_recompute`**

Rename the current `pub fn delete_edge(&mut self, s: u64, o: u64) -> DeleteOutcome`
body to a private `fn delete_edge_recompute(&mut self, s: u64, o: u64) -> DeleteOutcome`
(keep its doc comment, mark it "the recompute fallback"). It already removes
`(s, o)` from `base`; add the `bwd_base` removal from Task 1 there too if not
already shared — factor the base+bwd_base removal into a helper
`fn remove_base_edge(&mut self, s: u64, o: u64) -> bool` returning `was_base`,
and call it from both algorithms:

```rust
    /// Remove `(s, o)` from `base` and `bwd_base`. Returns whether it was an
    /// asserted base edge. Shared by both deletion strategies.
    fn remove_base_edge(&mut self, s: u64, o: u64) -> bool {
        let was_base = self
            .base
            .get_mut(&s)
            .map(|set| set.remove(&o))
            .unwrap_or(false);
        if !was_base {
            return false;
        }
        if self.base.get(&s).is_some_and(|set| set.is_empty()) {
            self.base.remove(&s);
        }
        if let Some(set) = self.bwd_base.get_mut(&o) {
            set.remove(&s);
            if set.is_empty() {
                self.bwd_base.remove(&o);
            }
        }
        true
    }
```

Update `delete_edge_recompute` to start with `if !self.remove_base_edge(s, o) { return DeleteOutcome::default(); }` instead of its inline base removal.

- [ ] **Step 5: Implement `delete_edge_support_counting` and the dispatcher**

```rust
    /// Number of base out-edges of `x` that witness reaching `y`
    /// (`z == y` or `closed(z, y)`). `closed(x, y) ⟺ support(x, y) ≥ 1`.
    /// O(base out-degree of `x`).
    fn support(&self, x: u64, y: u64) -> usize {
        match self.base.get(&x) {
            Some(outs) => outs
                .iter()
                .filter(|&&z| z == y || self.fwd.get(&z).is_some_and(|s| s.contains(&y)))
                .count(),
            None => 0,
        }
    }

    /// Output-sensitive decremental deletion (see PLAN-24-02). Removes `(s, o)`
    /// from the base and re-checks only the pairs that could have used `o` as a
    /// first hop from `s`, cascading backward along `bwd_base` through pairs
    /// that lose all support.
    fn delete_edge_support_counting(&mut self, s: u64, o: u64) -> DeleteOutcome {
        if !self.remove_base_edge(s, o) {
            return DeleteOutcome::default();
        }

        // Seed: pairs (s, y) that could have used the deleted first hop o,
        // i.e. y in {o} ∪ closed-fwd[o], that are currently closed.
        let mut targets: FxHashSet<u64> = self.fwd.get(&o).cloned().unwrap_or_default();
        targets.insert(o);
        let mut queue: Vec<(u64, u64)> = Vec::new();
        for &y in &targets {
            if self.fwd.get(&s).is_some_and(|set| set.contains(&y)) {
                queue.push((s, y));
            }
        }

        let mut withdrawn = Vec::new();
        // Guard against re-enqueue churn: a pair may be enqueued once per dead
        // witness; that is bounded by base in-degree and is the output-sensitive
        // cost. We do not dedup the queue (a survivor re-check is cheap and a
        // later witness death must be able to re-open it).
        while let Some((x, y)) = queue.pop() {
            // Already withdrawn or never closed: nothing to do.
            if !self.fwd.get(&x).is_some_and(|set| set.contains(&y)) {
                continue;
            }
            self.last_delete_probes += 1;
            if self.support(x, y) >= 1 {
                continue; // alternate witness survives
            }
            // Lost all support: withdraw and cascade to base predecessors.
            self.drop_closed(x, y);
            withdrawn.push((x, y));
            if let Some(preds) = self.bwd_base.get(&x) {
                for &w in preds {
                    queue.push((w, y));
                }
            }
        }

        // Survivor: the deleted edge (s, o) is still closed iff another base
        // path s ⇒ o remains.
        let survived = if self.fwd.get(&s).is_some_and(|set| set.contains(&o)) {
            vec![(s, o)]
        } else {
            Vec::new()
        };

        DeleteOutcome {
            withdrawn,
            survived,
        }
    }
```

Add the public dispatcher (this is the new `delete_edge`):

```rust
    /// Retract one asserted base edge `(s, o)` and return the [`DeleteOutcome`].
    /// Dispatches on the instance's [`DeleteStrategy`] (default
    /// `SupportCounting`, output-sensitive). See `delete_edge_support_counting`
    /// and `delete_edge_recompute`. Both maintain
    /// `closed == transitive_closure(base)` and produce identical results
    /// (verified by `tests/retraction_strategies_differential.rs`).
    pub fn delete_edge(&mut self, s: u64, o: u64) -> DeleteOutcome {
        self.last_delete_probes = 0;
        match self.strategy {
            DeleteStrategy::SupportCounting => self.delete_edge_support_counting(s, o),
            DeleteStrategy::Recompute => self.delete_edge_recompute(s, o),
        }
    }
```

(The bwd_base cleanup block sketched in Task 1 Step 3 for `delete_edge` is now
inside `remove_base_edge`; remove the duplicated inline version from Task 1 if it
was placed directly in the old `delete_edge` body.)

- [ ] **Step 6: Run the new test + full closure unit tests**

Run: `cargo test -p horndb-closure --lib`
Expected: PASS — `support_counting_matches_recompute_on_diamond` and all
existing `incremental::tests` pass (they exercise the default SupportCounting
path now).

- [ ] **Step 7: Run the existing retraction differential (default strategy)**

Run: `cargo test -p horndb-closure --test incremental_retraction`
Expected: PASS — 400 cases against the GraphBLAS oracle on the new default path.

- [ ] **Step 8: Commit**

```bash
git add crates/closure/src/closure/incremental.rs
git commit -m "closure: output-sensitive support-counting deletion + recompute fallback (SPEC-24 S2, #211)"
```

---

## Task 3: Dual-strategy differential proptest

Prove the two strategies are indistinguishable and both equal the GraphBLAS
bulk closure after every op — the strongest guard for the new algorithm.

**Files:**
- Create: `crates/closure/tests/retraction_strategies_differential.rs`

- [ ] **Step 1: Write the test**

```rust
//! SPEC-24 S2 differential test: the output-sensitive support-counting deletion
//! and the recompute fallback produce byte-identical closed sets and withdrawn
//! sets after every op in a random insert/delete sequence, and both equal the
//! from-scratch GraphBLAS closure of the current base.

use std::collections::BTreeSet;

use proptest::prelude::*;

use horndb_closure::closure::incremental::{DeleteStrategy, IncrementalTransitiveClosure};
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn grb_closure(n: u64, base: &BTreeSet<(u64, u64)>) -> BTreeSet<(u64, u64)> {
    if base.is_empty() {
        return BTreeSet::new();
    }
    let edges: Vec<(u64, u64)> = base.iter().copied().collect();
    let m = BoolMatrix::from_edges(n, &edges).unwrap();
    transitive_closure(&m)
        .unwrap()
        .extract_edges()
        .unwrap()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Insert(u64, u64),
    Delete(u64, u64),
}

fn op_strategy(n: u64) -> impl Strategy<Value = Op> {
    (0..n, 0..n, any::<bool>()).prop_map(|(s, o, ins)| {
        if ins {
            Op::Insert(s, o)
        } else {
            Op::Delete(s, o)
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn strategies_agree_and_match_grb(
        ops in {
            let n = 6u64;
            prop::collection::vec(op_strategy(n), 1..40)
        },
    ) {
        init_once().unwrap();
        let n = 6u64;

        let mut sc = IncrementalTransitiveClosure::new();
        sc.set_delete_strategy(DeleteStrategy::SupportCounting);
        let mut rc = IncrementalTransitiveClosure::new();
        rc.set_delete_strategy(DeleteStrategy::Recompute);
        let mut base: BTreeSet<(u64, u64)> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Insert(s, o) => {
                    sc.insert_edge(s, o);
                    rc.insert_edge(s, o);
                    base.insert((s, o));
                }
                Op::Delete(s, o) => {
                    let a = sc.delete_edge(s, o);
                    let b = rc.delete_edge(s, o);
                    let mut aw = a.withdrawn.clone();
                    let mut bw = b.withdrawn.clone();
                    aw.sort_unstable();
                    bw.sort_unstable();
                    prop_assert_eq!(&aw, &bw, "withdrawn differs after {:?}", op);
                    let mut asv = a.survived.clone();
                    let mut bsv = b.survived.clone();
                    asv.sort_unstable();
                    bsv.sort_unstable();
                    prop_assert_eq!(&asv, &bsv, "survived differs after {:?}", op);
                    base.remove(&(s, o));
                }
            }

            let got_sc: BTreeSet<(u64, u64)> = sc.edges().into_iter().collect();
            let got_rc: BTreeSet<(u64, u64)> = rc.edges().into_iter().collect();
            prop_assert_eq!(&got_sc, &got_rc, "closed sets diverge after {:?}", op);
            let reference = grb_closure(n, &base);
            prop_assert_eq!(&got_sc, &reference, "support-counting != GRB after {:?}", op);
        }
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-closure --test retraction_strategies_differential`
Expected: PASS (400 cases). If it fails, the shrunk counterexample pins the
exact op sequence — fix the algorithm (Task 2), not the test.

- [ ] **Step 3: Commit**

```bash
git add crates/closure/tests/retraction_strategies_differential.rs
git commit -m "closure: dual-strategy retraction differential proptest (SPEC-24 S2, #211)"
```

---

## Task 4: Output-sensitivity gate + seeded-base exact retraction test

A deterministic assertion that deletion cost is output-sensitive, plus the
acceptance test that a **base**-seeded closure retracts exactly (not
conservatively) — this needs the API from Task 5, so this task writes the
output-sensitivity half now and the seeded-base half after Task 5. Split into
two files/tests but one task for cohesion; do the probe test first.

**Files:**
- Create: `crates/closure/tests/retraction_output_sensitive.rs`

- [ ] **Step 1: Write the probe-count scaling test**

```rust
//! SPEC-24 acceptance #2: closure deletion is output-sensitive — a delete whose
//! closure delta and frontier are small inspects a bounded amount of work even
//! as the surrounding store grows. Uses the deterministic `last_delete_probes`
//! counter rather than wall-clock so the gate is CI-stable.

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;

/// Build N independent 2-edge chains a_i -> b_i -> c_i (disjoint node ids), plus
/// one extra redundant edge on chain 0 that, when deleted, withdraws nothing.
/// Deleting that redundant edge must inspect O(1) pairs regardless of N.
fn probes_for_store(n_chains: u64) -> usize {
    let mut c = IncrementalTransitiveClosure::new();
    for i in 0..n_chains {
        let a = i * 10;
        let b = i * 10 + 1;
        let d = i * 10 + 2;
        c.insert_edge(a, b);
        c.insert_edge(b, d);
    }
    // Chain 0 gets a redundant direct edge 0 -> 2 (already implied by 0->1->2).
    c.insert_edge(0, 2);
    // Deleting the redundant (0,2): (0,2) stays closed via 0->1->2, withdraws
    // nothing. Frontier = closed-fwd[2] ∪ {2} within chain 0 only.
    c.delete_edge(0, 2);
    c.last_delete_probes()
}

#[test]
fn deletion_probes_are_independent_of_store_size() {
    let small = probes_for_store(4);
    let large = probes_for_store(4_000);
    // Output-sensitive: the redundant-edge delete inspects the same bounded set
    // of pairs whether there are 4 chains or 4000. Allow a tiny constant slack.
    assert!(
        large <= small + 2,
        "probes must not scale with store: small={small}, large={large}"
    );
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p horndb-closure --test retraction_output_sensitive deletion_probes_are_independent_of_store_size`
Expected: PASS. (If `large` grows with N, the deletion is not output-sensitive —
fix Task 2, do not relax the bound.)

- [ ] **Step 3: Commit**

```bash
git add crates/closure/tests/retraction_output_sensitive.rs
git commit -m "closure: deterministic output-sensitivity gate for deletion (SPEC-24 S2, #211)"
```

(The seeded-base exact test is added in Task 6, after the API lands.)

---

## Task 5: `IncrementalClosureBackend::seed_base_edges` (exact warm seed)

**Files:**
- Modify: `crates/closure/src/sink.rs`
- Test: same file (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
/// Exact warm-store seed: seeding the TRUE asserted base (not the closed extent)
/// makes retracting a seeded edge exact, not conservative. Seed base {(1,2),(2,3)}
/// (closes 1->3); retract the seeded (2,3): withdraws (1,3) and (2,3) exactly.
#[test]
fn seed_base_edges_retracts_exactly() {
    let p = PredicateId(9);
    let mut backend = IncrementalClosureBackend::new();
    backend.seed_base_edges(p, &[(DictId(1), DictId(2)), (DictId(2), DictId(3))]);
    let out = backend
        .delete_transitive_edges(p, &[(DictId(2), DictId(3))])
        .expect("delete");
    assert_eq!(
        sorted(out.withdrawn),
        vec![(1, 3), (2, 3)],
        "base-seeded retraction is exact: (1,3) and (2,3) withdraw"
    );
    assert!(out.survived.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-closure --lib seed_base_edges_retracts_exactly`
Expected: FAIL — `no method named seed_base_edges`.

- [ ] **Step 3: Implement `seed_base_edges`**

In `impl IncrementalClosureBackend`, next to `seed_transitive_closure`:

```rust
    /// Seed predicate `p`'s retained closure from its **true asserted base**
    /// edges (not an already-closed extent), computing the closure at seed time.
    /// Unlike [`Self::seed_transitive_closure`] — which seeds a closed extent as
    /// a conservative base and can under-withdraw when a seeded edge is retracted
    /// — this records the genuine base, so retracting any seeded edge is
    /// **exact** (SPEC-24 S2). Costs one closure computation at seed; use the
    /// closed-extent seed only when the true base is unavailable. Replaces any
    /// existing state for `p`; writes nothing to a sink.
    pub fn seed_base_edges(&mut self, p: PredicateId, base_edges: &[(DictId, DictId)]) {
        let mut map = DenseIdMap::with_capacity(base_edges.len() * 2);
        let dense = map.intern_edges(base_edges);
        let closure = IncrementalTransitiveClosure::from_base_edges(dense);
        self.predicates.insert(p, PredicateState { map, closure });
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-closure --lib seed_base_edges_retracts_exactly`
Expected: PASS.

- [ ] **Step 5: Contrast test — closed-extent seed under-withdraws**

Add the paired test proving the OLD seed is conservative (documents the
difference; both are correct-but-different):

```rust
/// The closed-extent seed is conservative: seeding the CLOSED extent
/// {(1,2),(2,3),(1,3)} and retracting the seeded (2,3) leaves (1,3) reachable
/// via the redundant seeded (1,3), so it does NOT withdraw (1,3) — the exact
/// base seed above does. This pins the documented behavioural difference.
#[test]
fn seed_closed_edges_under_withdraws_vs_base_seed() {
    let p = PredicateId(9);
    let mut backend = IncrementalClosureBackend::new();
    backend.seed_transitive_closure(
        p,
        &[(DictId(1), DictId(2)), (DictId(2), DictId(3)), (DictId(1), DictId(3))],
    );
    let out = backend
        .delete_transitive_edges(p, &[(DictId(2), DictId(3))])
        .expect("delete");
    // (1,3) survives via the seeded direct (1,3): conservative, not exact.
    assert_eq!(sorted(out.withdrawn), vec![(2, 3)]);
}
```

- [ ] **Step 6: Run both + commit**

Run: `cargo test -p horndb-closure --lib seed_`
Expected: PASS.

```bash
git add crates/closure/src/sink.rs
git commit -m "closure: exact warm-store base seed (seed_base_edges) closing the closed-extent under-withdraw hole (SPEC-24 S2, #211)"
```

---

## Task 6: Seeded-base exact retraction acceptance test (integration)

Now add the acceptance-shaped test to `retraction_output_sensitive.rs`: a base
seed followed by a random retraction sequence matches the GraphBLAS closure of
the remaining base exactly (no conservatism).

**Files:**
- Modify: `crates/closure/tests/retraction_output_sensitive.rs`

- [ ] **Step 1: Write the test**

```rust
// (add these imports at the top of the file)
use std::collections::BTreeSet;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn grb(n: u64, base: &BTreeSet<(u64, u64)>) -> BTreeSet<(u64, u64)> {
    if base.is_empty() {
        return BTreeSet::new();
    }
    let edges: Vec<(u64, u64)> = base.iter().copied().collect();
    let m = BoolMatrix::from_edges(n, &edges).unwrap();
    transitive_closure(&m).unwrap().extract_edges().unwrap().into_iter().collect()
}

/// Acceptance #2 (seeded-base exact): seed a base with redundant edges, then
/// retract seeded edges one by one; the closure must exactly track the GRB
/// closure of the shrinking base — no under-withdrawal.
#[test]
fn base_seeded_retraction_is_exact() {
    init_once().unwrap();
    let n = 5u64;
    // Base includes the redundant (1,3) alongside 1->2->3.
    let seed = [(1u64, 2u64), (2, 3), (1, 3), (3, 4)];
    let mut c = IncrementalTransitiveClosure::from_base_edges(seed.iter().copied());
    let mut base: BTreeSet<(u64, u64)> = seed.iter().copied().collect();

    for &edge in &[(2u64, 3u64), (1, 3), (3, 4), (1, 2)] {
        c.delete_edge(edge.0, edge.1);
        base.remove(&edge);
        let got: BTreeSet<(u64, u64)> = c.edges().into_iter().collect();
        assert_eq!(got, grb(n, &base), "exact after deleting {:?}", edge);
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p horndb-closure --test retraction_output_sensitive base_seeded_retraction_is_exact`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/closure/tests/retraction_output_sensitive.rs
git commit -m "closure: seeded-base exact retraction acceptance test (SPEC-24 S2, #211)"
```

---

## Task 7: `TransitiveClosureRule::seed_base_edges` (SPEC-06 wrapper)

Expose the exact base seed on the incremental rule so a warm store with the true
asserted base gets exact retraction end-to-end.

**Files:**
- Modify: `crates/incremental/src/closure_plan.rs`
- Test: same file

- [ ] **Step 1: Write the failing test**

```rust
/// Warm-store exact seed: seeding the TRUE base {(1,2),(2,3)} and then retracting
/// the seeded (2,3) withdraws (1,3) and (2,3) exactly (contrast with
/// `seed_closed_edges`, which under-withdraws).
#[test]
fn transitive_rule_seed_base_edges_retracts_exactly() {
    let mut rule = TransitiveClosureRule::new(100);
    rule.seed_base_edges(&[(1, 2), (2, 3)]);
    let mut del: Zset<crate::types::TripleId> = Zset::new();
    del.add((2, 100, 3), -1);
    let mut got = rule.apply_retract_delta(&del);
    got.withdraw.sort_unstable();
    assert_eq!(got.withdraw, vec![(1, 100, 3), (2, 100, 3)]);
    assert!(got.promote.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-incremental --lib transitive_rule_seed_base_edges_retracts_exactly`
Expected: FAIL — `no method named seed_base_edges`.

- [ ] **Step 3: Implement the wrapper**

In `impl TransitiveClosureRule`, next to `seed_closed_edges`:

```rust
    /// Seed the retained closure from this predicate's **true asserted base**
    /// edges (dictionary-id `(s, o)` pairs), computing the closure at seed time.
    /// Unlike [`Self::seed_closed_edges`], which seeds an already-closed extent
    /// as a conservative base and can under-withdraw when a seeded edge is
    /// retracted, this records the genuine base so a later `apply_retract_delta`
    /// retracts **exactly** for any seeded edge (SPEC-24 S2). Costs one closure
    /// computation at seed; prefer it when the store can supply the asserted base.
    pub fn seed_base_edges(&mut self, base_edges: &[(u64, u64)]) {
        let edges: Vec<(DictId, DictId)> = base_edges
            .iter()
            .map(|&(s, o)| (DictId(s), DictId(o)))
            .collect();
        self.backend
            .seed_base_edges(PredicateId(self.predicate), &edges);
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p horndb-incremental --lib transitive_rule_seed_base_edges_retracts_exactly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/incremental/src/closure_plan.rs
git commit -m "incremental: TransitiveClosureRule::seed_base_edges for exact warm-store retraction (SPEC-24 S2, #211)"
```

---

## Task 8: Retraction A/B criterion bench

A local smoke bench comparing support-counting vs recompute on a fixed small
delta over a growing store. Numbers get recorded on hornbench later (not the
laptop); this task only wires the bench and runs it once locally to confirm it
builds and the support path is not slower on the sensitive shape.

**Files:**
- Create: `crates/closure/benches/closure_retraction.rs`
- Modify: `crates/closure/Cargo.toml` (add `[[bench]]`)

- [ ] **Step 1: Write the bench**

```rust
//! SPEC-24 S2 A/B: output-sensitive support-counting deletion vs the recompute
//! fallback, for a small-delta delete over a growing store. Run on hornbench for
//! recorded numbers (see docs/benchmarks.md); local runs are smoke only.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_closure::closure::incremental::{DeleteStrategy, IncrementalTransitiveClosure};

fn build(n_chains: u64) -> IncrementalTransitiveClosure {
    let mut c = IncrementalTransitiveClosure::new();
    for i in 0..n_chains {
        let a = i * 10;
        let b = i * 10 + 1;
        let d = i * 10 + 2;
        c.insert_edge(a, b);
        c.insert_edge(b, d);
    }
    c.insert_edge(0, 2); // redundant edge on chain 0
    c
}

fn bench_delete(cr: &mut Criterion) {
    let mut group = cr.benchmark_group("closure_retraction_redundant_delete");
    for &n in &[100u64, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("support_counting", n), &n, |bch, &n| {
            bch.iter_batched(
                || {
                    let mut c = build(n);
                    c.set_delete_strategy(DeleteStrategy::SupportCounting);
                    c
                },
                |mut c| {
                    let _ = c.delete_edge(0, 2);
                    c
                },
                criterion::BatchSize::SmallInput,
            );
        });
        group.bench_with_input(BenchmarkId::new("recompute", n), &n, |bch, &n| {
            bch.iter_batched(
                || {
                    let mut c = build(n);
                    c.set_delete_strategy(DeleteStrategy::Recompute);
                    c
                },
                |mut c| {
                    let _ = c.delete_edge(0, 2);
                    c
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_delete);
criterion_main!(benches);
```

- [ ] **Step 2: Register the bench**

Add to `crates/closure/Cargo.toml` after the other `[[bench]]` entries:

```toml
[[bench]]
name = "closure_retraction"
harness = false
```

- [ ] **Step 3: Smoke-run locally (do NOT record numbers)**

Run: `cargo bench -p horndb-closure --bench closure_retraction -- --warm-up-time 1 --measurement-time 2`
Expected: builds and runs; support_counting stays flat as `n` grows while
recompute climbs. (Recorded numbers come from hornbench per CLAUDE.md.)

- [ ] **Step 4: Commit**

```bash
git add crates/closure/benches/closure_retraction.rs crates/closure/Cargo.toml
git commit -m "closure: A/B retraction bench (support-counting vs recompute) (SPEC-24 S2, #211)"
```

---

## Task 9: Docs sync (architecture, integration notes, benchmarks, spec)

Reflect the delivered state. `TASKS.md` is intentionally NOT touched on the
branch (lock-serialized on `main`; the checkbox flip is a locked commit after
merge — sanctioned feature-branch exception, `AGENTS.md` → Keep the docs in
sync).

**Files:**
- Modify: `docs/architecture.md`
- Modify: `crates/closure/INTEGRATION-NOTES.md`
- Modify: `docs/benchmarks.md`
- Modify: `docs/specs/SPEC-24-incremental-stage2.md` (S2 status note)
- Modify: `docs/index.md` and `docs/plans` index if the plan needs listing

- [ ] **Step 1: architecture.md** — In the SPEC-06 rows (`| 3 | DBSP-style...`
  and `| Retraction semantics (F6) |`), update the closure-path clause: closure
  deletion is now **delta-incremental / output-sensitive** (SPEC-24 S2, #211,
  `PLAN-24-02`) — support-counting decremental with a retained recompute
  fallback — and exact warm-store retraction via `seed_base_edges`. Change the
  "the fully delta-incremental *closure* path is `SPEC-24` S2 (#211 ... planned)"
  wording to implemented, pointing at this plan. Keep #212–#217 as the remaining
  planned S-phases.

- [ ] **Step 2: INTEGRATION-NOTES.md** — Add a short subsection: deletion is
  output-sensitive by default (`DeleteStrategy::SupportCounting`), the recompute
  path is retained as `DeleteStrategy::Recompute` (per-instance, used as the
  differential oracle), and `seed_base_edges` gives exact seeded retraction
  vs the conservative `seed_transitive_closure`. Note the `last_delete_probes`
  counter as the output-sensitivity gate.

- [ ] **Step 3: benchmarks.md** — Add/adjust a closure-retraction row: target =
  output-sensitive (cost ∝ closure delta + frontier, not store); measured =
  "pending hornbench (bench `closure_retraction`)". Do not record laptop numbers.

- [ ] **Step 4: SPEC-24 S2** — no contract change; add a one-line status note at
  the S2 requirement or acceptance #2 that it is delivered by `PLAN-24-02`
  (support-counting + `seed_base_edges`), fallback retained.

- [ ] **Step 5: index/plan listing** — ensure `docs/plans` is discoverable if
  there is an index; add PLAN-24-02 if a plan index exists.

- [ ] **Step 6: Commit**

```bash
git add docs/architecture.md crates/closure/INTEGRATION-NOTES.md docs/benchmarks.md docs/specs/SPEC-24-incremental-stage2.md docs/index.md docs/plans
git commit -m "docs: SPEC-24 S2 output-sensitive closure deletion + exact seed delivered (#211)"
```

---

## Task 10: Full verification

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Clippy (what CI runs)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Full workspace tests**

Run: `cargo nextest run -p horndb-closure -p horndb-incremental`
Then: `cargo nextest run --workspace`
Expected: all green, including the new differential + output-sensitivity tests.

- [ ] **Step 4: Confirm the gate proptests specifically**

Run:
```
cargo test -p horndb-closure --test incremental_retraction
cargo test -p horndb-closure --test retraction_strategies_differential
cargo test -p horndb-closure --test retraction_output_sensitive
cargo test -p horndb-incremental --test closure_deltas_differential
```
Expected: all PASS.

- [ ] **Step 5: Final commit if fmt/clippy touched anything**

```bash
git add -A
git commit -m "chore: fmt/clippy for SPEC-24 S2 closure retraction (#211)" || true
```

---

## Self-review notes

- **Spec coverage.** SPEC-24 S2 has two bullets: output-sensitive deletion
  (Tasks 1–4, 8, with the recompute fallback retained per the risk note) and
  exact `seed_base_edges` (Tasks 5–7). Acceptance #2's three clauses map to:
  proptests green (Task 3 + existing `incremental_retraction` / `closure_deltas_differential`),
  output-sensitive shape (Task 4 probe gate + Task 8 bench), seeded-base exact
  (Tasks 6–7).
- **Fallback retained.** `DeleteStrategy::Recompute` keeps the original
  algorithm and is exercised every proptest run — satisfies the spec's
  "keep-the-recompute fallback per predicate".
- **No public break.** `DeleteOutcome`, `delete_transitive_edges`, and the sink
  boundary are unchanged; new methods are additive. Insert path is untouched.
- **Type consistency.** `DeleteStrategy` / `set_delete_strategy` /
  `last_delete_probes` / `seed_base_edges` names are used identically across
  closure, sink, and rule tasks.
