# SPEC-08 Integration Notes for `horndb-wcoj`

These notes describe call sites that **SPEC-03's plan** is responsible
for implementing.

## F2 — PlanAdvisor consultation

Before finalising a join order, the WCOJ planner should:

1. Construct a `horndb_ml::types::SubplanShape { n_patterns,
   n_vars, bound_vars }` from the candidate subplan.
2. Call `registry.plan_advisor().advise(&shape)` to obtain a
   `PlanAdvice`.
3. Treat every advice field as a **hint**: validate against the
   planner's own histograms before applying. If `estimated_cardinality`
   disagrees with the histogram by more than configured tolerance,
   discard the advice and use the histogram value.
4. NF2: if the advise call exceeds 1 ms p99 (measure via a rolling
   histogram), skip the advisor for that query and log a warning.

With `ml.enabled = false`, `advise()` returns `PlanAdvice::unadvised()`
and the planner uses histograms exclusively — bit-identical to a
no-ML build.

## Cost-based join planning (HDB-46, SPEC-23 phase 4, 2026-09-04)

- `Planner::choose(&Bgp, &dyn Stats) -> JoinSpec` replaces the fixed
  `wcoj_cutover == 4`. `Executor::for_bgp` takes the `Stats` seam directly;
  `Executor::for_spec` runs a caller-built `JoinSpec`.
- `JoinSpec` is the per-subplan IR: `Scan`, `HashJoin { build, probe }`,
  `Wcoj { patterns, var_order }`. It does not hard-code a binary/WCOJ switch,
  so a Free Join granularity knob can be added per node later.
- A whole-BGP `Wcoj` spec streams from `WcojExecutor`. Anything else runs on
  `executor/binary_hash.rs`, which materialises every node; `Scan` walks the
  pattern's preferred ordering and falls back to `Spo` if the source cannot
  serve it. `BinaryHashExecutor::new` still builds the left-deep
  scans-only oracle the differential fuzzer depends on — keep it free of
  WCOJ nodes.
- Cost constants (`cost.rs`): `HASH_BUILD_WEIGHT = 8`, `MATERIALIZE_WEIGHT = 8`
  (charged per row on sub-nodes of a hash tree only; whole-BGP WCOJ streams),
  `HYBRID_MARGIN = 2` (a hybrid tree must be that much cheaper than whole-BGP
  WCOJ to be chosen), `UNBOUND_PRED_DIVISOR = 25`, `AGM_MAX_PATTERNS = 5`.
  Uncalibrated; the AGM bound only caps `card()`. `planner.rs`:
  `MAX_DP_PATTERNS = 5`, then greedy over units (one core per connected
  component plus single patterns). Because the hash side materialises, the
  model rarely picks a hash join over the leapfrog for connected BGPs — the
  variable order inside the WCOJ node is where the win lives today (HDB-108
  q3, `?customer` first via the first-variable shortlist sweep in
  `cost.rs::OrderSearch`).
- A WCOJ node's cost is infinite only for a single pattern (that is a scan,
  `Planner::unit` picks it); several patterns over one variable are a real
  intersection and are costed like any node (`plan_ab.rs::attr_star`).
- Multi-pattern cardinalities use `StatsEstimator::estimate_bgp_fast`
  (product of per-pattern bounds, no characteristic-set walk) so a
  10-pattern star plans in ~100 µs; `estimate_bgp` stays for EXPLAIN and
  single patterns.
- `Stats::is_informed()` (default `true`, `false` for `ZeroStats`) gates the
  search; uninformed stats, and any single-pattern BGP, give one WCOJ node
  in degree order, the old production plan.
- `OrderedTripleIter::active_run_ready(depth)` (default `false`) must be
  cheap and allocation-free: `try_arm_simd` checks it on both sides before
  building either `active_run`. See `docs/architecture/wcoj.md` §7.1 for the
  cliff this avoids.
- An empty BGP is the join identity: `Executor::for_bgp` yields one solution
  with no bindings.
