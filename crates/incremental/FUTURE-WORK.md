# horndb-incremental — Deferred Work

Stage 1 of SPEC-06 deliberately ships a narrow slice. This file
catalogues what is **out** of Stage 1, in priority order for Stage 2,
with the SPEC-06 requirement ID and the trigger for promotion.

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
- **Still Stage 2**: (a) a *fully delta-incremental* retraction path —
  the current path recomputes the whole rule closure on every
  retraction-containing tick rather than threading negative
  multiplicities through each bilinear (the DBSP correctness theorem,
  McSherry/Ryzhyk/Tannen PVLDB 2023 §3); and (b) **closure-path
  retraction** — F5's `ClosureRule` is stateful and insertion-only, so
  closure-inferred rows are never withdrawn and the rule diff leaves
  them untouched (see the F5 entry below).

### F7 — In-flight reader visibility (MVCC)
- **Done (#46)**: refcounted `Snapshot` handles (`Circuit::snapshot()`,
  `crate::snapshot::Snapshot`) pin a consistent `(asserted ∪ derived)` view at
  a logical time across multiple ticks; readers and writers never block. The
  presence view is built lazily and cached: a state-changing `tick()` only
  invalidates the cache in O(1) (so steady-state writes stay delta-sized), and
  `snapshot()` is amortized O(1) (`Arc` clone) but pays one
  O(|asserted| + |derived|) build on the first acquire after a write.
- **Still deferred (parent #6)**: backing the snapshot interface onto SPEC-02
  per-tuple storage MVCC, and point queries against partially-applied in-flight
  deltas mid-tick.
- **Possible optimization (parent #6)**: make the first post-write `snapshot()`
  O(1) too by maintaining the version incrementally with structural sharing
  (persistent/COW Z-set) instead of rebuilding the presence set. Deferred until
  post-write reader latency on a warm store is shown to matter — the lazy build
  keeps the write hot path delta-sized, which is the priority for SPEC-06.

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
  closure. F6 (above) delivered rule-path retraction, but the closure
  operator stays insertion-only, so this is now its own follow-up rather
  than F6-blocked — it needs the deletion half of SPEC-05's incremental
  closure. Also outstanding: closure↔rule cross-feedback *within* a single
  tick (closure output feeding rule bodies and vice versa); non-transitive
  closure shapes.

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
- **Naïve nested-loop bilinear join**: the reference rule fixtures
  use O(n²) nested loops. SPEC-04 codegen will emit hash/sort-merge
  variants per rule shape; the trait surface is unchanged.
- **Differential test equivalence is set-semantics**: now tightened.
  With F6 landed (#45), acceptance #4 (`tests/acceptance_differential.rs`)
  checks multiplicity equality and covers interleaved insert+retract
  (was support-set comparison + insertion-only).
