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
- **Differential test equivalence is set-semantics**: acceptance #4
  compares the (asserted ∪ derived) support sets, not multiplicities.
  When F6 lands, tighten to multiplicity equality.
