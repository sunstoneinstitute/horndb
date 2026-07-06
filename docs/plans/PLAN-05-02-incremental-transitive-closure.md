---
status: executed
date: 2026-06-01
scope: "SPEC-05 F6 — Incremental Insertion-Path Transitive Closure"
---

# SPEC-05 F6 — Incremental Insertion-Path Transitive Closure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an insertion-only incremental transitive-closure path to `horndb-closure` so a newly inserted edge updates only the affected slice of the closure (`R' = R ∪ (backward-reach(s) × forward-reach(o))`) instead of re-running the full GraphBLAS closure, and writes only the newly inferred triples to the sink.

**Architecture:** The initial bulk closure stays on GraphBLAS (`closure/transitive.rs`). A new dense-index-space structure `IncrementalTransitiveClosure` (forward + backward adjacency maps over an already-closed relation) folds in inserted edges pointwise — a single-edge update of a transitively-closed relation is a rank-1 outer-product OR, an inherently sparse set operation for which GraphBLAS `mxm` is overkill. A thin `IncrementalClosureBackend` owns per-predicate `DenseIdMap` + `IncrementalTransitiveClosure`, maps `DictId` edges in/out, and writes only the delta to the `TripleSink` (GraphBLAS-derived, F5). Correctness is pinned by a differential proptest against the existing GraphBLAS `transitive_closure` (SPEC-05 acceptance #4 shape).

**Tech Stack:** Rust 1.88, `rustc_hash::FxHashMap`/`FxHashSet`, the existing `horndb-closure` GraphBLAS wrapper, `rand` 0.8 (small_rng) + a hand-rolled property loop for differential testing, `criterion` 0.5 for the bench.

**Why insertion-only / what stays deferred:** Deletion needs SPEC-06 DBSP machinery (SPEC-06 is insertion-only at Stage 1). GPU backend is SPEC-09. LAGraph is Stage-2. These remain deferred under parent epic #5.

---

## File Structure

- **Create `crates/closure/src/closure/incremental.rs`** — `IncrementalTransitiveClosure` dense-space algorithm (the heart). Owns `fwd`/`bwd` adjacency. Pure Rust, no FFI. Unit + property tested.
- **Modify `crates/closure/src/closure/mod.rs`** — add `pub mod incremental;`.
- **Modify `crates/closure/src/sink.rs`** — add `IncrementalClosureBackend` (retained per-predicate state, `DictId`↔dense mapping, delta writeback to `TripleSink`). Reuses `EquivClasses` for sameAs, mirrors `write_closure`.
- **Modify `crates/closure/src/lib.rs`** — re-export, update the module doc's "Future work" note (incremental insertion now shipped; deletion still Stage-2).
- **Create `crates/closure/tests/incremental.rs`** — differential test (incremental ≡ GraphBLAS full closure) + end-to-end sink delta test.
- **Create `crates/closure/benches/incremental.rs`** + **modify `crates/closure/Cargo.toml`** — criterion bench: incremental single-edge insert vs full recompute.
- **Modify `docs/architecture.md`** — flip SPEC-05 "Incremental closure updates (F6)" row planned → partially implemented (insertion-only); note F5 pairing.
- **Modify `docs/benchmarks.md`** — add SPEC-05 incremental-closure bench row.
- **Modify `TASKS.md`** — mark #42 increment delivered in the #5 breakdown note (parent stays `[v]`).

---

## Task 1: `IncrementalTransitiveClosure` core (dense space)

**Files:**
- Create: `crates/closure/src/closure/incremental.rs`
- Modify: `crates/closure/src/closure/mod.rs`
- Test: `crates/closure/src/closure/incremental.rs` (inline `#[cfg(test)]`)

**Algorithm.** The structure represents the **strict** transitive closure (matching `transitive_closure`, which omits the identity) as forward adjacency `fwd: FxHashMap<u64, FxHashSet<u64>>` (`y ∈ fwd[x]` ⟺ `x` reaches `y` in ≥1 hop) and its transpose `bwd`. Inserting edge `(s, o)` into an already-closed relation:

```text
B = { x : (x,s) ∈ R } ∪ { s }      // bwd[s] ∪ {s}
F = { y : (o,y) ∈ R } ∪ { o }      // fwd[o] ∪ {o}
for x in B, for y in F:
    if y ∉ fwd[x]: add (x,y) to fwd[x] and bwd[y]; record (x,y) in delta
```

Correct because `R` is closed before the insert: every new path through `(s,o)` is `[old path to s] + (s,o) + [old path from o]`, exactly `B × F`. Cycles produce self-loops (e.g. inserting `(s,o)` when `o` already reaches `s` puts `s ∈ F` and `o ∈ B`, yielding `(s,s)`,`(o,o)`), matching `transitive_closure`'s behaviour on cycles (`triangle_cycle_closes_to_full_3x3`).

- [ ] **Step 1: Write the failing unit tests**

Create `crates/closure/src/closure/incremental.rs` with the test module only (the type doesn't exist yet — that's the point):

```rust
//! Insertion-only incremental transitive closure (SPEC-05 F6).
//!
//! The initial bulk closure is computed on GraphBLAS (`closure/transitive.rs`).
//! Once a relation is transitively closed, inserting a single edge `(s, o)`
//! adds exactly the cross product of everything that reaches `s` (inclusive)
//! with everything reachable from `o` (inclusive) — a rank-1 outer-product OR.
//! That is an inherently sparse pointwise update, so we maintain it directly in
//! Rust (forward + backward adjacency) rather than paying GraphBLAS `mxm` cost
//! per edge. Deletion is **not** handled here — it needs SPEC-06 DBSP deltas.

use rustc_hash::{FxHashMap, FxHashSet};

/// Strict transitive closure over dense `u64` indices, maintained incrementally
/// under edge insertion. "Strict" = no implicit identity; a self-loop `(x,x)`
/// appears only when `x` lies on a cycle, matching
/// [`crate::closure::transitive::transitive_closure`].
#[derive(Default, Clone)]
pub struct IncrementalTransitiveClosure {
    fwd: FxHashMap<u64, FxHashSet<u64>>,
    bwd: FxHashMap<u64, FxHashSet<u64>>,
    nnz: usize,
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
                (1, 1), (1, 2), (1, 3),
                (2, 1), (2, 2), (2, 3),
                (3, 1), (3, 2), (3, 3),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn from_closed_seeds_existing_state() {
        // Seed with an already-closed 1->2->3 (so 1->3 present), then extend.
        let seed = vec![(1, 2), (1, 3), (2, 3)];
        let mut c = IncrementalTransitiveClosure::from_closed_edges(seed.iter().copied());
        assert_eq!(c.nnz(), 3);
        let mut delta = c.insert_edge(3, 4);
        delta.sort_unstable();
        assert_eq!(delta, vec![(1, 4), (2, 4), (3, 4)]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile (type incomplete)**

Run: `cargo test -p horndb-closure --lib closure::incremental 2>&1 | tail -20`
Expected: compile error — no method `insert_edge`/`edges`/`nnz`/`from_closed_edges` on `IncrementalTransitiveClosure`.

(First register the module so it compiles at all — do Step 3's `mod.rs` edit before running, or expect "module not found".)

- [ ] **Step 3: Register the module**

In `crates/closure/src/closure/mod.rs`, add the module declaration:

```rust
//! Closure algorithms — transitive, sub-class, sub-property.

pub mod incremental;
pub mod schema;
pub mod transitive;
```

- [ ] **Step 4: Implement `IncrementalTransitiveClosure`**

Add to `crates/closure/src/closure/incremental.rs` (above the `#[cfg(test)]` module):

```rust
impl IncrementalTransitiveClosure {
    /// Empty closure.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from a set of edges that is **already transitively closed** (e.g.
    /// the output of [`crate::closure::transitive::transitive_closure`]). The
    /// caller guarantees closure; this constructor does not re-close.
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
        // B = {x : x reaches s} ∪ {s}; F = {y : o reaches y} ∪ {o}.
        let mut b: Vec<u64> = self.bwd.get(&s).map(|set| set.iter().copied().collect()).unwrap_or_default();
        b.push(s);
        let mut f: Vec<u64> = self.fwd.get(&o).map(|set| set.iter().copied().collect()).unwrap_or_default();
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

    /// Insert many edges (folded one at a time so later edges observe earlier
    /// contributions) and return the combined delta across all of them.
    pub fn insert_edges<I: IntoIterator<Item = (u64, u64)>>(&mut self, edges: I) -> Vec<(u64, u64)> {
        let mut delta = Vec::new();
        for (s, o) in edges {
            delta.extend(self.insert_edge(s, o));
        }
        delta
    }
}
```

Note on the `b`/`f` materialization: we copy `B` and `F` into `Vec`s before the
double loop because the loop mutates `self.fwd`/`self.bwd`, which would
otherwise alias the borrowed sets. `B`/`F` are slices of the closure, not the
whole matrix — that is the "affected slice" SPEC-05 F6 asks for.

- [ ] **Step 5: Run unit tests to verify they pass**

Run: `cargo test -p horndb-closure --lib closure::incremental 2>&1 | tail -20`
Expected: all 6 tests PASS.

- [ ] **Step 6: fmt + clippy the new module**

Run: `cargo fmt -p horndb-closure && cargo clippy -p horndb-closure --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/closure/src/closure/incremental.rs crates/closure/src/closure/mod.rs
git commit -m "feat(closure): incremental transitive-closure core (SPEC-05 F6)"
```

---

## Task 2: Differential test vs GraphBLAS full closure

**Files:**
- Create: `crates/closure/tests/incremental.rs`

This is the SPEC-05 acceptance #4 shape for the incremental path: the incremental result must equal the from-scratch GraphBLAS closure, with no missing and no spurious edges, over many random graphs and insertion orders.

- [ ] **Step 1: Write the differential test**

Create `crates/closure/tests/incremental.rs`:

```rust
//! Differential test for the incremental transitive closure (SPEC-05 F6).
//!
//! For many random graphs and random insertion orders, the incrementally
//! maintained closure must equal the from-scratch GraphBLAS closure
//! (`transitive_closure`). Two scenarios are covered:
//!   (a) from empty — insert every edge one at a time;
//!   (b) seeded — close a prefix on GraphBLAS, then insert the rest
//!       incrementally.

use std::collections::BTreeSet;

use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn random_edges(n: usize, density_per_node: usize, rng: &mut SmallRng) -> Vec<(u64, u64)> {
    let mut set: BTreeSet<(u64, u64)> = BTreeSet::new();
    for s in 0..n {
        for _ in 0..density_per_node {
            let o = rng.gen_range(0..n);
            set.insert((s as u64, o as u64));
        }
    }
    set.into_iter().collect()
}

fn grb_closure(n: usize, edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    if edges.is_empty() {
        return BTreeSet::new();
    }
    let m = BoolMatrix::from_edges(n as u64, edges).unwrap();
    let star = transitive_closure(&m).unwrap();
    star.extract_edges().unwrap().into_iter().collect()
}

#[test]
fn incremental_from_empty_matches_grb_closure() {
    init_once().unwrap();
    for (seed, n, density) in [(1u64, 8usize, 2usize), (2, 15, 3), (3, 30, 2), (4, 60, 3)] {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut edges = random_edges(n, density, &mut rng);
        let reference = grb_closure(n, &edges);

        // Insert in a shuffled order to exercise order-independence.
        edges.shuffle(&mut rng);
        let mut inc = IncrementalTransitiveClosure::new();
        inc.insert_edges(edges.iter().copied());
        let got: BTreeSet<(u64, u64)> = inc.edges().into_iter().collect();

        assert_eq!(
            got, reference,
            "from-empty mismatch seed={seed} n={n} density={density}\n\
             only in incremental: {:?}\nonly in reference: {:?}",
            got.difference(&reference).collect::<Vec<_>>(),
            reference.difference(&got).collect::<Vec<_>>()
        );
    }
}

#[test]
fn seeded_then_incremental_matches_grb_closure() {
    init_once().unwrap();
    for (seed, n, density) in [(11u64, 10usize, 2usize), (12, 20, 3), (13, 40, 2)] {
        let mut rng = SmallRng::seed_from_u64(seed);
        let edges = random_edges(n, density, &mut rng);
        if edges.len() < 4 {
            continue;
        }
        let split = edges.len() / 2;
        let (prefix, rest) = edges.split_at(split);

        // Seed the incremental structure from a real GraphBLAS closure of the
        // prefix, then insert the remaining edges incrementally.
        let seeded = grb_closure(n, prefix);
        let mut inc = IncrementalTransitiveClosure::from_closed_edges(seeded.iter().copied());
        inc.insert_edges(rest.iter().copied());
        let got: BTreeSet<(u64, u64)> = inc.edges().into_iter().collect();

        let reference = grb_closure(n, &edges);
        assert_eq!(
            got, reference,
            "seeded mismatch seed={seed} n={n} density={density}\n\
             only in incremental: {:?}\nonly in reference: {:?}",
            got.difference(&reference).collect::<Vec<_>>(),
            reference.difference(&got).collect::<Vec<_>>()
        );
    }
}
```

- [ ] **Step 2: Add `rand`'s `SliceRandom` availability**

`rand` 0.8 is already a dev-dependency with `small_rng`. `SliceRandom`/`shuffle` are in the default `rand` features — no Cargo change needed. Confirm by building.

- [ ] **Step 3: Run the differential test**

Run: `cargo test -p horndb-closure --test incremental 2>&1 | tail -25`
Expected: both tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/closure/tests/incremental.rs
git commit -m "test(closure): differential incremental-vs-GraphBLAS closure (SPEC-05 F6)"
```

---

## Task 3: `IncrementalClosureBackend` — retained per-predicate state + delta writeback

**Files:**
- Modify: `crates/closure/src/sink.rs`
- Modify: `crates/closure/src/lib.rs`
- Test: `crates/closure/tests/incremental.rs` (append an end-to-end test)

This is the boundary surface: it maps `DictId` edges into dense space per
predicate, folds them into a retained `IncrementalTransitiveClosure`, and writes
**only the delta** triples to the `TripleSink` (tagged GraphBLAS-derived, F5),
mirroring the existing `write_closure`.

- [ ] **Step 1: Write the failing end-to-end test**

Append to `crates/closure/tests/incremental.rs`:

```rust
use std::sync::Mutex;

use horndb_closure::sink::{IncrementalClosureBackend, TripleSink};
use horndb_closure::types::{DictId, PredicateId, Triple};

#[derive(Default)]
struct VecSink {
    triples: Mutex<Vec<Triple>>,
}

impl TripleSink for VecSink {
    fn bulk_insert_inferred(
        &self,
        triples: &mut dyn Iterator<Item = Triple>,
    ) -> Result<u64, anyhow::Error> {
        let mut guard = self.triples.lock().unwrap();
        let before = guard.len();
        guard.extend(triples);
        Ok((guard.len() - before) as u64)
    }
}

#[test]
fn incremental_backend_writes_only_the_delta() {
    let sink = VecSink::default();
    let mut backend = IncrementalClosureBackend::default();
    let p = PredicateId(42);

    // First insert 1->2: only (1,2) is new.
    let w1 = backend
        .insert_transitive_edges(p, &[(DictId(1), DictId(2))], &sink)
        .unwrap();
    assert_eq!(w1, 1);

    // Insert 2->3: new closure edges are (2,3) and (1,3).
    let w2 = backend
        .insert_transitive_edges(p, &[(DictId(2), DictId(3))], &sink)
        .unwrap();
    assert_eq!(w2, 2);

    // Insert 3->4: new are (3,4),(2,4),(1,4).
    let w3 = backend
        .insert_transitive_edges(p, &[(DictId(3), DictId(4))], &sink)
        .unwrap();
    assert_eq!(w3, 3);

    let triples = sink.triples.lock().unwrap();
    let mut pairs: Vec<(u64, u64)> = triples.iter().map(|t| (t.s.0, t.o.0)).collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![(1, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 4)]
    );
    for t in triples.iter() {
        assert_eq!(t.p, p);
    }
}

#[test]
fn incremental_backend_dedups_reinserted_edges() {
    let sink = VecSink::default();
    let mut backend = IncrementalClosureBackend::default();
    let p = PredicateId(7);
    backend
        .insert_transitive_edges(p, &[(DictId(1), DictId(2))], &sink)
        .unwrap();
    // Re-inserting the same edge writes nothing new.
    let again = backend
        .insert_transitive_edges(p, &[(DictId(1), DictId(2))], &sink)
        .unwrap();
    assert_eq!(again, 0);
}
```

- [ ] **Step 2: Run to verify it fails (type missing)**

Run: `cargo test -p horndb-closure --test incremental incremental_backend 2>&1 | tail -20`
Expected: compile error — `IncrementalClosureBackend` not found in `horndb_closure::sink`.

- [ ] **Step 3: Implement `IncrementalClosureBackend`**

Add to `crates/closure/src/sink.rs`. First extend the imports at the top:

```rust
use rustc_hash::FxHashMap;

use crate::closure::incremental::IncrementalTransitiveClosure;
```

(Keep the existing `use` lines; add these alongside. `rustc_hash` is already a
dependency of the crate — see `dense_id.rs`.)

Then append, after `BackendImpl`:

```rust
/// Per-predicate retained closure state for the incremental path (SPEC-05 F6).
#[derive(Default)]
struct PredicateState {
    map: DenseIdMap,
    closure: IncrementalTransitiveClosure,
}

/// Insertion-only incremental closure backend. Unlike [`BackendImpl`], which
/// recomputes the whole closure from the full edge set on every call, this
/// retains per-predicate closure state and folds in only the newly inserted
/// edges, writing **only the delta** triples to the sink (SPEC-05 F6).
///
/// Insertion only — deletion needs SPEC-06 DBSP deltas and is out of scope.
#[derive(Default)]
pub struct IncrementalClosureBackend {
    predicates: FxHashMap<PredicateId, PredicateState>,
    sameas: EquivClasses,
}

impl IncrementalClosureBackend {
    pub fn new() -> Self {
        let _ = init_once();
        Self::default()
    }

    /// Insert `new_edges` into predicate `p`'s transitive closure and write the
    /// newly inferred triples to `sink`. Returns the number of triples the sink
    /// reports written. Edges already implied by the existing closure produce
    /// no output.
    pub fn insert_transitive_edges(
        &mut self,
        p: PredicateId,
        new_edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        if new_edges.is_empty() {
            return Ok(0);
        }
        let state = self.predicates.entry(p).or_default();
        let dense = state.map.intern_edges(new_edges);
        let delta = state.closure.insert_edges(dense);
        if delta.is_empty() {
            return Ok(0);
        }
        let map = &state.map;
        let mut iter = delta.iter().filter_map(|&(s, o)| {
            let s_dict = map.to_dict(DenseIdx(s))?;
            let o_dict = map.to_dict(DenseIdx(o))?;
            Some(Triple { s: s_dict, p, o: o_dict })
        });
        sink.bulk_insert_inferred(&mut iter)
    }

    /// Union `owl:sameAs` pairs (shared with the bulk backend's semantics).
    pub fn add_sameas(&mut self, pairs: &[(DictId, DictId)]) {
        for &(a, b) in pairs {
            self.sameas.union(a, b);
        }
    }

    /// Borrow the equivalence-class state.
    pub fn equiv_classes(&self) -> &EquivClasses {
        &self.sameas
    }
}
```

Ensure `DenseIdx` is imported in `sink.rs` (the existing `use crate::types::{...}`
already lists `DenseIdx, DictId, PredicateId, Triple` — confirm; if `DenseIdx`
is absent, add it).

- [ ] **Step 4: Re-export and update the lib doc note**

In `crates/closure/src/lib.rs`, update the "Future work" bullet for incremental
update to reflect the insertion path now shipping:

```rust
//! - Incremental update (SPEC-05 F6): **insertion path implemented** via
//!   [`sink::IncrementalClosureBackend`] /
//!   [`closure::incremental::IncrementalTransitiveClosure`] — a single-edge
//!   insert updates only the affected slice (backward-reach × forward-reach)
//!   instead of re-closing. **Deletion/retraction is still Stage 2** (needs
//!   SPEC-06 DBSP deltas).
```

(Leave the other future-work bullets — GPU, LAGraph, `(min,+)`, routing
heuristic, `GrB_Matrix_dup` — unchanged.)

- [ ] **Step 5: Run the end-to-end tests**

Run: `cargo test -p horndb-closure --test incremental 2>&1 | tail -25`
Expected: all four tests in the file PASS (2 differential + 2 backend).

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt -p horndb-closure && cargo clippy -p horndb-closure --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/closure/src/sink.rs crates/closure/src/lib.rs crates/closure/tests/incremental.rs
git commit -m "feat(closure): IncrementalClosureBackend writes only the delta (SPEC-05 F6)"
```

---

## Task 4: Criterion bench — incremental insert vs full recompute

**Files:**
- Create: `crates/closure/benches/incremental.rs`
- Modify: `crates/closure/Cargo.toml`

Substantiates F6's value: appending an edge to an existing closure should be far
cheaper incrementally than recomputing the whole closure on GraphBLAS.

- [ ] **Step 1: Add the bench target to `Cargo.toml`**

In `crates/closure/Cargo.toml`, after the existing `[[bench]]` entries, add:

```toml
[[bench]]
name = "incremental"
harness = false
```

- [ ] **Step 2: Write the bench**

Create `crates/closure/benches/incremental.rs`:

```rust
//! SPEC-05 F6 bench: incremental single-edge insert vs full GraphBLAS
//! recompute on a transitivity chain.

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

/// Edges of a chain 0->1->2->...->(n-1), plus its transitive closure size.
fn chain(n: u64) -> Vec<(u64, u64)> {
    (0..n - 1).map(|i| (i, i + 1)).collect()
}

fn bench_incremental_vs_full(c: &mut Criterion) {
    init_once().unwrap();
    let n: u64 = 2_000;
    let base = chain(n); // 0..n-1 chain
    let new_edge = (n - 1, n); // appends one node, extending the chain

    let mut group = c.benchmark_group("spec05_incremental_append");

    // Full recompute: build matrix of base+new_edge and close from scratch.
    group.bench_function("full_recompute", |b| {
        let mut all = base.clone();
        all.push(new_edge);
        b.iter(|| {
            let m = BoolMatrix::from_edges(n + 1, &all).unwrap();
            let star = transitive_closure(&m).unwrap();
            black_box(star.nvals().unwrap());
        });
    });

    // Incremental: pre-close the base once, then time only the single insert.
    group.bench_function("incremental_insert", |b| {
        let m = BoolMatrix::from_edges(n + 1, &base).unwrap();
        let closed = transitive_closure(&m).unwrap().extract_edges().unwrap();
        b.iter_batched(
            || IncrementalTransitiveClosure::from_closed_edges(closed.iter().copied()),
            |mut inc| {
                let delta = inc.insert_edge(new_edge.0, new_edge.1);
                black_box(delta.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_incremental_vs_full);
criterion_main!(benches);
```

- [ ] **Step 3: Build the bench (compile only — keep CI fast)**

Run: `cargo build -p horndb-closure --bench incremental 2>&1 | tail -10`
Expected: compiles clean.

- [ ] **Step 4: Run the bench once to capture numbers for the docs**

Run: `cargo bench -p horndb-closure --bench incremental 2>&1 | tail -25`
Expected: `incremental_insert` is dramatically faster than `full_recompute`
(orders of magnitude on a 2,000-node chain). Record the two medians for Task 5.

- [ ] **Step 5: clippy the bench**

Run: `cargo clippy -p horndb-closure --benches -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/closure/benches/incremental.rs crates/closure/Cargo.toml
git commit -m "bench(closure): incremental insert vs full recompute (SPEC-05 F6)"
```

---

## Task 5: Documentation sync (architecture.md, docs/benchmarks.md, TASKS.md)

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/benchmarks.md`
- Modify: `TASKS.md`

Per CLAUDE.md "Keep the docs in sync" — same change as the code.

- [ ] **Step 1: Flip the architecture.md SPEC-05 row**

In `docs/architecture.md`, change the "Incremental closure updates (F6)" row
(currently `**planned**`) to:

```markdown
| Incremental closure updates (F6) — insertion path | **partially implemented** | `closure/incremental.rs` (`IncrementalTransitiveClosure`) + `sink.rs` (`IncrementalClosureBackend`): a single-edge insert updates only the affected slice (backward-reach(s) × forward-reach(o)) and writes only the delta to the sink. Differential proptest vs GraphBLAS full closure (`tests/incremental.rs`). **Deletion/retraction deferred** — needs SPEC-06 DBSP deltas ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)/[#42](https://github.com/sunstoneinstitute/horndb/issues/42)). |
```

Also update the SPEC-06 row at line ~245 "Closure-operator deltas (F5) | **planned** | Pairs with SPEC-05 incremental closure." — leave `**planned**` but append: "Insertion-side SPEC-05 incremental closure now ships ([#42](https://github.com/sunstoneinstitute/horndb/issues/42)); the SPEC-06 delta-feed pairing is still planned."

- [ ] **Step 2: Add the docs/benchmarks.md row**

In `docs/benchmarks.md`, under the SPEC-05 section (near the `transitive`/`sameas`
bench rows ~line 157), add a row to the bench-inventory table:

```markdown
| `benches/incremental.rs` | `horndb-closure` | SPEC-05 F6 incremental insert vs full recompute. |
```

And add a measured row to the SPEC-05 measurements table (near line 87), using
the numbers captured in Task 4 Step 4 (fill in the real medians):

```markdown
| `spec05_incremental_append` — single-edge append on a 2,000-node chain | incremental ≪ full recompute | this PR (macOS dev workstation): incremental_insert **<X> µs** vs full_recompute **<Y> ms** (~<Z>×). Insertion-only F6; differential-proven equal to GraphBLAS closure. |
```

- [ ] **Step 3: Mark #42 delivered in the TASKS.md #5 breakdown (parent stays `[v]`)**

In `TASKS.md`, in the `## MEDIUM` body entry for SPEC-05 closure, update the
breakdown note so #42 reads as delivered while the parent remains `[v]`:

Change the breakdown bullet to:

```markdown
  - **Epic breakdown (2026-06-01, tracked under [#5](https://github.com/sunstoneinstitute/horndb/issues/5)):**
    [#42](https://github.com/sunstoneinstitute/horndb/issues/42) SPEC-05 F6
    incremental insertion-path transitive closure — **first increment,
    delivered 2026-06-01**: `IncrementalTransitiveClosure`
    (`crates/closure/src/closure/incremental.rs`) + `IncrementalClosureBackend`
    (`crates/closure/src/sink.rs`) update only the affected slice on insert and
    write only the delta; differential proptest vs the GraphBLAS full closure.
    Deferred under this parent until shippable: deletion/retraction half of F6
    (blocked on SPEC-06 DBSP deltas, #6); GPU GraphBLAS backend (SPEC-09);
    LAGraph adoption (Stage-2 eval); `GrB_Matrix_dup` fast-clone, `(min,+)`
    cost-aware semiring, and nnz-threshold routing heuristic (Stage-2 perf
    tuning). Parent stays `[v]` until the increments close.
```

(Leave the index line and the body heading at `[v]` with the wip tag — the
parent epic is not done.)

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md docs/benchmarks.md TASKS.md
git commit -m "docs(closure): record SPEC-05 F6 incremental closure (#42) in architecture/benchmarks/tasks"
```

---

## Task 6: Full-workspace verification gate

**Files:** none (verification only)

- [ ] **Step 1: fmt check**

Run: `cargo fmt --all -- --check`
Expected: clean (no diff).

- [ ] **Step 2: clippy the workspace**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings. (First run pulls in oxrocksdb-sys — slow; reused from the
shared target dir.)

- [ ] **Step 3: workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: all green, including the new `closure::incremental` unit tests and the
`incremental` integration test.

- [ ] **Step 4: harness-first sanity (SPEC-00 rule)**

The incremental path is a new internal capability not yet wired into the
`harness` engine or any graded suite, so no harness subset changes are required
for this increment (the bulk closure path remains the graded one). Confirm the
existing real-engine harness still builds:

Run: `cargo build -p horndb-harness --bin harness --features real-engine 2>&1 | tail -5`
Expected: compiles clean.

- [ ] **Step 5: No commit (gate only).** If any step is red, fix before opening the PR.

---

## Self-Review Notes

- **Spec coverage (SPEC-05 F6):** "recompute only the affected slice — forward-reachable from new edge target, backward-reachable to new edge source" → Task 1 `insert_edge` computes exactly `B = bwd(s)∪{s}`, `F = fwd(o)∪{o}`, updates `B×F`. ✅ Writeback of inferred triples tagged GraphBLAS-derived (F5) → Task 3 reuses the `TripleSink` contract. ✅ Differential equality with rule-firing/closure reference (acceptance #4 shape) → Task 2. ✅
- **Out of scope (correctly deferred):** deletion (SPEC-06), GPU (SPEC-09), LAGraph (Stage-2), routing heuristic, `GrB_Matrix_dup`. Documented in lib.rs + TASKS.md.
- **Type consistency:** `IncrementalTransitiveClosure::{new, from_closed_edges, insert_edge, insert_edges, edges, nnz, is_empty}` used identically across Tasks 1–4. `IncrementalClosureBackend::{new (default), insert_transitive_edges, add_sameas, equiv_classes}` used identically in Task 3 tests and impl. `DenseIdx`/`DictId`/`PredicateId`/`Triple` match `types.rs`.
- **No placeholders:** every code block is complete; the only fill-ins are the measured bench medians in Task 5 Step 2 (real numbers captured in Task 4 Step 4), which is correct (you cannot know them before running).
