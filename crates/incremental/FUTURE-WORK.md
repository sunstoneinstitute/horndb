# horndb-incremental — Deferred Work

Stage 1 of SPEC-06 deliberately ships a narrow slice. This file
catalogues what is **out** of Stage 1, in priority order for Stage 2,
with the SPEC-06 requirement ID and the trigger for promotion.

> **Stage 2 is now specified.** `docs/specs/SPEC-24-incremental-stage2.md`
> (epic [#186](https://github.com/sunstoneinstitute/horndb/issues/186))
> turns this catalog into requirements S1–S8 with acceptance criteria,
> decomposed into phase sub-issues
> [#210](https://github.com/sunstoneinstitute/horndb/issues/210)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217).
> For planning, the spec wins; this file stays as the per-item detail log.

## Stage 2 (next milestone)

### F6 — Correct retraction across joins — DELIVERED (rule path)
- **Done (2026-06-17, #45)**: `Circuit::tick` now has two regimes.
  Insertion-only ticks keep the unchanged forward semi-naïve path. Any
  tick containing a retraction (`mult < 0`) recomputes the **set-semantics
  rule closure** of the post-delta `asserted_base` (`recompute_rule_closure`)
  and diffs it against the prior rule-derived rows, tracked via a
  `rule_attr: BTreeMap<TripleId, RuleId>` map: newly-derivable rows are
  added and published as positive `RuleInferred`; no-longer-derivable rows
  are withdrawn (zeroed in `derived_base`) and published as negative
  `RuleInferred`. This is order-independent and correct for arbitrary
  `(triple, ±k)`, and sidesteps the unbounded path-count divergence that
  pure derivation-count Z-set accumulation hits on cyclic recursive rules.
  See `crates/incremental/src/circuit.rs`.
- **Promotion test**: SPEC-06 acceptance #3 — insert 10K, retract 10K,
  store bit-identical (modulo timestamps) to pre-insertion — passes
  (`tests/retraction.rs::insert_10k_retract_10k_bit_identical`).
- **Closure-path retraction — DELIVERED (2026-06-18, #5)**: see the F5
  entry below. A `ClosureInferred` row whose base support is retracted is
  now withdrawn.
- **Delta-incremental rule path — DELIVERED (2026-07-20, #210,
  `PLAN-24-01`)**: `Circuit::tick` runs one unified incremental fixpoint.
  A tick with retractions runs a two-phase overdelete / re-derive
  (DRed-style) pass driven by per-row per-rule one-step weight traces
  (`rule_weights`) with an incremental distinct; net rule events are
  published per tick. The Stage-1 recompute survives only as a config-gated
  fallback (`Circuit::new_with_recompute_fallback()`) and as the
  differential-test oracle. See
  `docs/plans/PLAN-24-01-delta-incremental-rule-retraction.md`.
- **Still Stage 2 (SPEC-24 S2, #211)**: closure-path retraction still
  recomputes base-reachability over the affected source region per
  retracted edge rather than threading negative deltas end-to-end.

### F7 — In-flight reader visibility (MVCC) — DELIVERED (storage-backed)
- **Done (#46, then SPEC-24 S6 / #215)**: `Circuit::attach_store(store, graphs)`
  binds the reader view to the store the S4 wiring writes (default graph +
  derived-mirror graph); `Circuit::snapshot()` pins that store's current commit
  version and returns `Some(Snapshot)`. Acquire is O(1) — an `Arc` clone plus a
  tier pin-count bump — and the circuit materializes no presence set of its own.
  `Snapshot::logical_time()` is the storage commit version (ADR-0018: one
  clock). Readers and writers never block; a pin survives later ticks.
- **Still deferred**: point queries against partially-applied in-flight deltas
  mid-tick, and enforcing ADR-0018's "one tick, one storage batch" on the engine
  write path (today an Update commits its base batch and its derived mirror as
  two storage versions, so a snapshot between them sees base rows without their
  consequences).

### F5 — Closure-operator deltas (SPEC-05 integration) — DELIVERED (insertion + retraction)
- **Done (2026-06-01, #44)**: `Circuit::add_closure_plan(Box<dyn ClosureRule>)`
  registers a closure operator. `TransitiveClosureRule`
  (`crates/incremental/src/closure_plan.rs`) wraps SPEC-05's
  `IncrementalClosureBackend`; on each tick it folds the asserted insertion
  delta into the retained per-predicate closure and emits only the newly
  inferred triples, published as `DerivationKind::ClosureInferred`. Differential
  test (`tests/closure_deltas_differential.rs`) pins it against the full
  `BackendImpl` recompute.
- **Closure-path retraction — Done (2026-06-18, #5)**:
  `ClosureRule::apply_retract_delta` (default no-op; overridden by
  `TransitiveClosureRule`) consumes the negative-only asserted delta and calls
  SPEC-05's `IncrementalClosureBackend::delete_transitive_edges`, returning the
  closure edges to withdraw. `Circuit::tick` runs the closure-retraction pass
  **before** the rule recompute on retraction-containing ticks: each withdrawn
  edge is dropped from `closure_support` unconditionally, and zeroed in
  `derived_base` with a negative `ClosureInferred` published **only** when the
  row is not still rule-owned (`rule_attr`) or otherwise supported — the dual of
  the Finding-2 overlap-retention logic. Tests: `tests/closure_retraction.rs`
  (chain break, diamond second-path retention, re-assert round-trip) and the
  rewritten `tests/retraction_closure.rs`.
- **Mixed-tick insert+retract closure→rule — Done (2026-06-18, #5)**: on a tick
  that simultaneously retracts one support edge and inserts a replacement path,
  the closure INSERTION pass now runs BEFORE the rule recompute (the closure
  retraction pass still runs first), so the recompute sees the post-tick closure
  and a rule consequence whose closure support is still entailed via the
  replacement path survives. The end-of-tick insertion pass is skipped on
  retraction ticks (shared helper `Circuit::run_closure_insertion_pass`) so it
  never runs twice. Test:
  `tests/retraction_closure.rs::mixed_tick_insert_replacement_path_keeps_rule_consequence`.
- **Done (SPEC-24 S3, #212)**: change-feed net-delta reconciliation. Derived
  emissions accumulate in a tick-local Z-set keyed by `(triple, kind)` and only
  non-zero nets publish at tick end, so the same-tick closure withdraw+re-add
  transient is gone;
  `tests/closure_retraction.rs::mixed_tick_replacement_path_final_state_correct`
  now asserts its absence.
  - **Forward note**: netting discards the intra-tick derivation sequence. A
    consumer that needs *why* a row moved — SPEC-27 provenance — needs the
    PRE-net stream, so it will want a second tap beside `pending_derived`
    (or a provenance sink inside `emit_derived`), not a change to the feed
    contract. Do not "fix" the netting to serve it.
- **Still Stage 2**: a fully delta-incremental closure-retraction path (no
  affected-region recompute); **exact warm-store seeded-edge retraction** — a
  rule seeded via `TransitiveClosureRule::seed_closed_edges` uses the *closed*
  extent as a conservative base, so `apply_retract_delta` is exact for edges
  inserted via `apply_insert_delta` and **sound (but may under-withdraw)** when
  retracting against seeded support; recovering the true asserted base needs a
  base-seed variant; closure→rule cross-feedback *within a PURE INSERTION tick* (a closure
  edge feeding a rule body in the same tick it is first derived — the insertion
  pass still runs after the rule forward pass on insertion-only ticks) and
  rule→closure feedback within a tick; non-transitive closure shapes.

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
- **Backpressure on change feed**: done (SPEC-24 S3, #212).
  `subscribe_bounded(capacity, LagPolicy)` bounds the per-subscriber buffer;
  `DisconnectSlow` (default) drops a lagging subscriber and counts it,
  `Block` backpressures the tick. `subscribe()` stays unbounded as an explicit
  opt-out. See `INTEGRATION-NOTES.md`.
- **`HashMap` vs `BTreeMap` in `Zset`**: BTreeMap was chosen for
  deterministic iteration (change-feed ordering). If profiling shows
  iteration is not the bottleneck and lookup dominates, swap to a
  randomised-state HashMap with a stable iteration adapter.
- **Differential test equivalence is set-semantics**: now tightened.
  With F6 landed (#45), acceptance #4 (`tests/acceptance_differential.rs`)
  checks multiplicity equality and covers interleaved insert+retract
  (was support-set comparison + insertion-only).
