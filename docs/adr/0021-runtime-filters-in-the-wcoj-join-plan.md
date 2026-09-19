# ADR-0021: Runtime filters live in the WCOJ join plan, switched through a reserved `PassId`

**Status:** Proposed. This record asks Stig (CTO) for one decision: which of the three options in "Options considered" HornDB takes, and whether the narrowing of SPEC-23 §5.6 and §7.2 is acceptable. Nothing in it is accepted or implemented.

**Date:** 2026-09-19

**Source:** Written for Worklode task HDB-240 (escalated from HDB-206, the runtime-filter task of PLAN-23-05 / HDB-PLAN-58). Narrows SPEC-23 §5.6 and §7.2; follows from the placement SPEC-23 §5.5 and §8 #3 already record for cost-based join planning (HDB-46).

## Context

A runtime filter (also called sideways information passing) is a join optimization: evaluate the small side of a join first, collect the join-key values it produced, and use that set to skip rows on the large side before they reach the join. SPEC-23 §5.6 reserved it as "a later `PassId`", that is a logical pass in the `horndb-sparql` pass registry, and PLAN-23-05 Task 3 (HDB-206) repeats that while also saying "target WCOJ intermediate bindings first". The worker found those two halves now name different layers, and that two of the task's inputs do not exist.

**Cost-based join planning is not a pass.** PLAN-23-04 (HDB-46) put it in `horndb-wcoj`: `Planner::choose(bgp, stats) -> JoinSpec` (`crates/wcoj/src/planner.rs`) returns a tree of `Scan` / `Wcoj` / `HashJoin { build, probe }` nodes (`crates/wcoj/src/plan.rs`), called at execution time from `HornBackend` through `Executor::for_bgp` (`crates/wcoj/src/executor/mod.rs`). `PassId::JoinPlanning` is an enum variant that `standard_passes()` (`crates/sparql/src/plan/pass.rs`) deliberately does not wire; SPEC-23 §5.5 and §8 #3 record this and reserve the variant for algebra-level join ordering, which is not built. A filter built from a hash join's build side and pushed into its probe side is therefore a decision about a `JoinSpec`, and `horndb-wcoj` has no `PassId`, no `PlanCtx`, and no `disabled_passes` set. SPEC-23 §7.2 ("every pass is individually disable-able via config/pragma ... a regression can be bisected to a single `PassId`") does not reach it as written.

**There is no seam.** No bloom filter, set filter, skip filter, semi-join or anti-join exists in `crates/wcoj` or `crates/sparql`. The hash-join tree evaluator (`crates/wcoj/src/executor/binary_hash.rs::eval`) evaluates `build` and `probe` independently and joins the two materialized row sets; nothing flows from one side to the other.

**The semi/anti rewrites have no input.** `FILTER EXISTS` / `NOT EXISTS` does not translate in `horndb-sparql` (`Expr` in `crates/sparql/src/algebra/mod.rs` excludes EXISTS; the W3C `exists/` cases are excluded in `harness/selected.toml`; the gap is HDB-134). The only anti-join is `MINUS`, an algebra-level node above the BGP. ClickHouse's any-join → semi/anti and join → `IN` rewrites act on those algebra-level shapes, and the algebra-level join ordering they would sit in is the unbuilt `JoinPlanning` pass.

**The win is unmeasurable from a laptop.** PLAN-23-05's unblock list requires "a representative RDF star-join / `FILTER EXISTS` workload identified to prove a runtime-filter win". None is named, and benchmark numbers for this repo come only from `hornbench`.

What already exists and can carry the design:

- `JoinSpec::HashJoin { build, probe }` is exactly the boundary a runtime filter acts on, and `Planner::choose` already assigns build sides in a late pass (`assign_build_sides`).
- A leapfrog `Wcoj` node intersects one sorted iterator per pattern at each variable's elimination depth. A sorted, distinct set of `TermId`s is a unary relation and can join that intersection at the depth of the variable it constrains. Inside one `Wcoj` node the leapfrog already passes bindings sideways through its seeks, so there is nothing to add there.
- `Planner` already carries a bisection switch outside the pass registry: `fixed_cutover` (`HORNDB_WCOJ_CUTOVER`).
- `PassId` already holds a variant no pass implements (`JoinPlanning`), and its module comment says the spare variants exist "so a pragma can name them". The pragma is `PRAGMA disable-pass=<id>` (`crates/sparql/src/parser.rs::strip_plan_pragmas`), parsed into `PlanCtx::disabled_passes` at parse time.
- The differential fuzzer (`crates/wcoj/tests/differential_fuzz.rs`) already runs the planned `JoinSpec` and a hand-built hybrid against the binary-hash oracle on every case.
- A star-join workload with a selective side already exists and is already measured on `hornbench`: trainmarks `q3_join_3_entities` (`docs/benchmarks.md`), whose shape is pinned in `crates/wcoj/tests/planner_choice.rs::q3_shape_binds_selective_customer_before_order` (a four-pattern star on `?order` joined to `?customer :country :Norway`).

## Options considered

**A. A filter on `JoinSpec::HashJoin`, decided by `Planner::choose`, honored by the executor, switched through a reserved `PassId`.** The planner names the probe-side variable to filter; the executor builds an exact sorted set from the build side's key column and skips probe rows that miss it. Disabling is one `PassId` in the existing pragma namespace. Cost: one field on `JoinSpec`, one branch in the evaluator, one unary iterator in the leapfrog, one `PassId` variant, one fuzzer variant. Limit: the filter fires only at a `HashJoin` boundary, so a whole-BGP leapfrog plan (the uninformed-stats path, and any hybrid that does not beat the leapfrog by `HYBRID_MARGIN`) gets no filter.

**B. A real logical pass in `horndb-sparql`.** Split a BGP into two BGPs in the logical plan and insert a filter node between them. Rejected: the pass would have to repeat `Planner::choose`'s build/probe decision at a layer that has no `Stats`, and splitting a BGP before the WCOJ planner sees it defeats the cyclic-core rule (a core is never split by a hash join). It is the right shape only for the algebra-level rewrites, which have no input today.

**C. Do nothing until a `hornbench` profile shows a hybrid plan's probe side dominating a query.** Honest, and the cheapest option. Cost: the seam stays unbuilt, HDB-206 stays blocked on evidence that nobody is set up to collect, and SPEC-23 §5.6 stays a reservation with no owner.

**Recommendation: A**, sliced so the first shippable unit is the seam plus its switch, proved correct locally, with the measurement as a separate `hornbench` task.

## Decision (proposed)

### 1. `horndb-wcoj` owns runtime filters

A runtime filter is part of the per-BGP join plan. `JoinSpec::HashJoin` grows a field (working name `filter: Option<Var>`) naming one variable both sides bind. `Planner::choose` sets it in the same late pass that assigns build sides; when set, the executor evaluates `build`, collects the distinct values of that variable as a sorted `Vec<TermId>`, and evaluates `probe` under that set:

- a `Scan` node drops triples whose value at the variable's position is not in the set;
- a `Wcoj` node adds the set as one more sorted iterator at the variable's elimination depth;
- a nested `HashJoin` passes the set down to whichever child binds the variable.

The set is exact, not a bloom filter: the build side is already fully materialized in memory, and the leapfrog needs a sorted, seekable set anyway. A bloom filter is a later change, taken only if a measured set is too large to be cache-resident. One variable per join is the first slice; any subset of the join key is a sound filter, so this loses nothing but selectivity.

Correctness is by construction: a probe row whose key is not in the build set cannot survive the hash join, so filtering it earlier changes no result. The proof is the existing differential fuzzer with the filter on, and `tests/planner_choice.rs` pinning which plans get a filter.

### 2. The switch is a reserved `PassId`; §7.2 is narrowed, not dropped

`PassId` gains `RuntimeFilter` (`"runtime-filter"`), listed in `PassId::ALL` and never wired into `standard_passes()`, the same status `JoinPlanning` has. `PRAGMA disable-pass=runtime-filter` therefore parses today's way into `PlanCtx::disabled_passes`. `HornBackend` reads that flag when it builds the `Planner` for a query (`Planner` grows a `runtime_filters: bool` next to `fixed_cutover`) and the switch reaches the executor the same way the cancel token does. With the flag off the planner never sets `filter`, so the `JoinSpec` and every result are byte-identical to today: plan neutrality when off, as SPEC-23 §7.2 and this plan's goal require.

This narrows §7.2 for one item. "Individually disable-able via pragma" and "a regression bisects to a single `PassId`" both hold. "Debug builds validate the IR after each pass" has no logical IR to validate here; the differential fuzzer with the filter on is the validation for this `PassId`. Config-level disabling is not wired for any pass today (only the pragma is); that is unchanged.

Any later planner decision that lives below the BGP boundary (Free Join / COLT in PLAN-23-05 Task 6, for one) takes the same route: a `Planner` flag, named by a reserved `PassId`.

### 3. The any-join → semi/anti and join → `IN` rewrites are out of this plan

They need `FILTER EXISTS` to translate (HDB-134) and an algebra-level join ordering pass to sit in (`PassId::JoinPlanning`, unbuilt). When both exist they are a real logical pass in `horndb-sparql`, and belong to whichever plan builds algebra-level join ordering. PLAN-23-05 Task 3 drops them.

### 4. The measurement is its own `hornbench` task, with the workload named now

The workload PLAN-23-05 asked for is trainmarks `q3_join_3_entities`, already in `docs/benchmarks.md`, plus the LDBC SPB-256 nightly and `four_cycle` for no-regression. The `FILTER EXISTS` half of that prerequisite is dropped: no such workload can exist until HDB-134 lands. The measurement task runs on `hornbench` only and has one gate before it times anything: confirm the workload's BGP gets a `HashJoin` plan with informed stats there. If it gets a whole-BGP leapfrog, record that and stop; no runtime filter can help a plan with no hash-join boundary, and the next lever is the cost model (charging the probe side less when a filter is available), not this seam.

## Consequences

- `+` HDB-206 becomes implementable now, locally verifiable, and neutral when off.
- `+` One switch namespace. A bisection over `PassId::ALL` still covers every optimizer decision, including the ones the WCOJ planner makes.
- `+` No new crate dependency; `sparql → wcoj` is unchanged.
- `−` SPEC-23 §5.6 said "a later `PassId`" meaning a logical pass; under this ADR the `PassId` is a switch, not a pass. §7.2's IR-validation clause does not apply to it.
- `−` The win is bounded to hybrid plans and unproven. Option C remains the honest fallback if the measurement task finds no hash-join boundary on the workload.
- `−` The cost model does not yet know a filter exists, so the planner will not choose a hybrid *because* a filter would make it cheap. That is a follow-up, gated on the measurement.

## Related

- Narrows: SPEC-23 §5.6 (runtime filters) and §7.2 (pass legibility). Follows: SPEC-23 §5.5 and §8 #3 as amended by HDB-46.
- Plans: PLAN-23-05 (HDB-PLAN-58) Task 3 (HDB-206) and Task 7 (HDB-210).
- Blocked prerequisites for the dropped half: HDB-134 (`EXISTS` / `NOT EXISTS`).
- Code the design reuses: `crates/wcoj/src/{plan,planner}.rs`, `crates/wcoj/src/executor/{mod,binary_hash,wcoj}.rs`, `crates/sparql/src/plan/pass.rs`, `crates/sparql/src/parser.rs`, `crates/wcoj/tests/{differential_fuzz,planner_choice}.rs`.
