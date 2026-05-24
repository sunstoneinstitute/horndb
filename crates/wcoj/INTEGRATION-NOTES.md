# SPEC-08 Integration Notes for `reasoner-wcoj`

These notes describe call sites that **SPEC-03's plan** is responsible
for implementing.

## F2 — PlanAdvisor consultation

Before finalising a join order, the WCOJ planner should:

1. Construct a `reasoner_ml::types::SubplanShape { n_patterns,
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
