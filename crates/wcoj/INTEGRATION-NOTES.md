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
- Cost constants (`cost.rs`): `HASH_BUILD_WEIGHT = 4`, `MATERIALIZE_WEIGHT = 1`,
  `UNBOUND_PRED_DIVISOR = 25`. Uncalibrated; the AGM bound only caps
  `card()`. Because the hash side materialises, the model rarely picks a hash
  join over the leapfrog for connected BGPs — the variable order inside the
  WCOJ node is where the win lives today (HDB-108 q3).
- `Stats::is_informed()` (default `true`, `false` for `ZeroStats`) gates the
  search; uninformed stats give one WCOJ node in degree order, the old
  production plan.
