---
status: executed
date: 2026-06-18
scope: "SPEC-05 closure-path retraction (deletion half of F6)"
---

# Plan — SPEC-05 closure-path retraction (deletion half of F6)

Tracking: epic [#5](https://github.com/sunstoneinstitute/horndb/issues/5)
(SPEC-05 closure). This is the **deletion/retraction half of F6**, previously
deferred and blocked on SPEC-06 retraction (#6/#45), which has now landed.

## Goal

Make the incremental transitive closure correct under **edge retraction**:
when an asserted base edge `(s, o)` for a transitive predicate is withdrawn,
the closure must withdraw exactly those inferred pairs `(x, y)` that are no
longer derivable from the remaining base edges — never an edge still supported
by another path. This delivers closure-path retraction through the SPEC-06
`Circuit`, so a `ClosureInferred` row is withdrawn when its support is gone.

Non-DRed by design (per SPEC-05 F6: "Deletion uses SPEC-06's DBSP machinery
rather than DRed"). We track the **asserted base edges** alongside the closure
and recompute reachability over the *affected region* from the remaining base,
diffing against the prior closure to produce the negative delta. This is the
DBSP "recompute the affected slice and diff" primitive, mirroring the rule-path
retraction recompute-and-diff already in `Circuit::recompute_rule_closure`.

## Current state (verified in code)

- `IncrementalTransitiveClosure` (`crates/closure/src/closure/incremental.rs`)
  stores only the **closed** edge set (`fwd`/`bwd` adjacency). It has no record
  of which edges are asserted (base) vs inferred, so it cannot tell whether a
  closure pair survives a base-edge removal. `insert_edge` does a rank-1
  outer-product OR; there is no `delete_edge`.
- `IncrementalClosureBackend` (`crates/closure/src/sink.rs`) is insertion-only;
  per-predicate `PredicateState { map, closure }`.
- `TransitiveClosureRule` (`crates/incremental/src/closure_plan.rs`) implements
  `ClosureRule::apply_insert_delta`, filtering for positive multiplicities and
  ignoring negatives.
- `Circuit::tick` (`crates/incremental/src/circuit.rs`) feeds closure plans only
  the **positive-only** asserted delta (`asserted_delta_for_closure`) and never
  withdraws `ClosureInferred` rows; `closure_support` keeps them alive across
  rule-path retraction.

## Design

### Layer 1 — SPEC-05 closure crate (the algorithm)

Extend `IncrementalTransitiveClosure` to retain the **base (asserted) edge set**
in addition to the closed set:

- New field `base: FxHashMap<u64, FxHashSet<u64>>` (forward base adjacency) — the
  asserted edges only. (`fwd`/`bwd` stay the *closed* adjacency.)
- `from_closed_edges` keeps the existing closed-set semantics but **cannot**
  reconstruct the base; add a parallel constructor that takes the base and
  closes it. For the seeded warm-store path we need base edges, so add
  `from_base_edges(base) -> Self` that interns the base and computes the closure
  via `insert_edges`. Keep `from_closed_edges` for the legacy
  "already-closed, no retraction needed" callers (it leaves `base` empty, which
  is fine because those callers never retract).
- `insert_edge` records `(s,o)` in `base` (in addition to the existing closure
  update).
- New `delete_edge(s, o) -> Vec<(u64, u64)>`: remove `(s,o)` from `base`. If it
  was not a base edge, return empty (idempotent). Otherwise recompute the
  closure over the **affected region** and return the withdrawn pairs (the
  negative delta, as positive `(x,y)` tuples the caller negates):
  1. Affected sources `B = bwd_base*(s) ∪ {s}` and targets `F = fwd_base*(o) ∪ {o}`
     — but those are reachability over the *old* closure. The pairs that *could*
     lose support are exactly `{(x, y) : x ∈ (closed-bwd[s] ∪ {s}),
     y ∈ (closed-fwd[o] ∪ {o})}` that were present before. Candidate set.
  2. Recompute reachability for those candidate pairs over the **post-delete
     base** (BFS/DFS from each affected source restricted to nodes in the
     candidate region, or simply recompute the closure of the affected source
     set against the full remaining base — correctness first).
  3. The withdrawn delta = candidate pairs present in the old closure but NOT
     reachable in the recomputed closure. Update `fwd`/`bwd`/`nnz` to drop them.
- `delete_edges<I>(edges) -> Vec<(u64,u64)>`: fold deletions (apply one at a
  time so each sees prior removals), combined negative delta.

Correctness anchor: after any sequence of inserts/deletes, the closed set must
equal `transitive_closure(base)` (the bulk GraphBLAS reference). This is the
differential invariant we test with a proptest, mirroring `tests/incremental.rs`.

Then `IncrementalClosureBackend::delete_transitive_edges(p, edges, sink)`:
mirror `insert_transitive_edges` but call `delete_edges`, and write the withdrawn
triples to the sink as **retractions**. The `TripleSink` API only has
`bulk_insert_inferred` returning a count; closure retraction needs to express
*negative* multiplicity. Two options:
  (a) add `bulk_retract_inferred(&mut dyn Iterator<Item = Triple>) -> Result<u64>`
      to `TripleSink` (default-impl `unimplemented!`-free by giving it a body
      that errors, OR make it required and update the one in-tree impl), or
  (b) keep the sink insertion-only and have the backend method *return* the
      withdrawn `Vec<Triple>` for the caller to apply with the right sign.
Choose **(b)**: it is the smaller blast radius (the only sink impl is the
SPEC-06 `VecSink`), keeps `TripleSink` a pure insertion boundary, and the
SPEC-06 layer is exactly where the +/- sign lives (Z-set multiplicities). The
backend's `delete_transitive_edges(p, edges)` returns
`Result<Vec<(DictId, DictId)>>` (the withdrawn closure edges); no sink param.
For symmetry and to keep the sink-writing insert path unchanged, leave
`insert_transitive_edges` as-is.

### Layer 2 — SPEC-06 incremental (the wiring)

- Extend the `ClosureRule` trait with
  `apply_retract_delta(&mut self, asserted_delta: &Zset<TripleId>) -> Vec<TripleId>`
  returning the closure triples to **withdraw** (callers negate). Default-impl
  it to return `Vec::new()` so any future closure rule that is insertion-only
  still compiles; `TransitiveClosureRule` overrides it.
- `TransitiveClosureRule::apply_retract_delta`: filter the delta for
  `mult < 0` edges of this predicate, call
  `backend.delete_transitive_edges(pid, edges)`, map withdrawn `(s,o)` to
  `TripleId`s.
- `Circuit::tick`: build a **negative-only** closure delta alongside the
  positive one. On retraction-containing ticks, after the rule recompute-and-
  diff, run `apply_retract_delta` over each closure plan; for each withdrawn
  triple, if it is currently a `closure_support` row (and not otherwise
  supported), zero it in `derived_base`, drop it from `closure_support`, and
  publish a negative `ClosureInferred`. Critically, the withdrawal must respect
  rule support: a triple that is *also* rule-derived (still in the post-recompute
  `rule_attr`) must NOT be zeroed — only its closure ownership lapses. This is
  the dual of the existing Finding-2 logic.
- Ordering within the tick: the rule recompute already seeds from
  `asserted_base ∪ closure_support`. Closure retraction shrinks
  `closure_support`, which can in turn invalidate rule consequences that joined
  against the withdrawn closure edge. To keep the fixed point correct, run the
  closure-retraction pass **before** the rule recompute on retraction ticks, so
  `recompute_rule_closure` sees the already-shrunk `closure_support`. (Insertion
  closure stays after the rule pass, unchanged.) Verify the existing
  `retraction_closure.rs` Finding-1/Finding-2/ghost tests still pass — their
  semantics change: Finding-1 previously asserted `(c,SC,e)` *persists* after
  retracting `(d,SC,e)`; with closure-path retraction it must now be **withdrawn**
  (the base edge it derived from is gone), and `(a,TYPE,e)` withdrawn with it.
  Those tests must be **updated** to the new correct behavior and re-justified.

### Differential test (the gate)

`crates/closure/tests/incremental_retraction.rs`: proptest a random sequence of
inserts and deletes; after each op assert
`closure.edges()  == transitive_closure(current_base)` (sorted), using the bulk
GraphBLAS path as oracle. This is the SPEC-05 acceptance-style differential
("no missing, no spurious") for the retraction path.

`crates/incremental/tests/closure_retraction.rs`: end-to-end through `Circuit` —
assert a chain, tick, retract a base edge, tick, assert the `ClosureInferred`
rows that lost support are withdrawn from `derived_base` and a negative
`ClosureInferred` appears in the feed; rows still supported persist.

Update `crates/incremental/tests/retraction_closure.rs` to the new semantics.

## Deferred (document, do not implement here)

- Fully delta-incremental closure retraction (threading negative deltas without
  any affected-region recompute) — still Stage 2.
- GPU GraphBLAS backend (SPEC-09), LAGraph adoption, `(min,+)` semiring,
  nnz-threshold routing — unchanged Stage-2/3 deferrals under #5.
- Closure↔rule cross-feedback *within a single tick* beyond what the existing
  pass does — Stage 2 (FUTURE-WORK.md).

## Verification

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p horndb-closure`
- `cargo test -p horndb-incremental`
- `cargo test --workspace`

## Docs to update in the same PR (per CLAUDE.md, minus TASKS.md)

- `docs/architecture.md`: SPEC-05 "Incremental closure updates (F6)" row →
  deletion path implemented; SPEC-06 "Closure-operator deltas (F5)" and
  "Retraction semantics (F6)" rows → closure-path retraction implemented;
  the row-3 capability-matrix line.
- `crates/closure/INTEGRATION-NOTES.md` / `crates/incremental/FUTURE-WORK.md`:
  move closure-path retraction from "Still Stage 2" to delivered.
- `crates/incremental/CLAUDE.md`: note closure-path retraction now works.
- (TASKS.md #5 line + GitHub issue handled on `main` post-merge, per next-task.)
