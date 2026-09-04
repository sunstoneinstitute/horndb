---
status: executed
date: 2026-09-04
scope: "Phase 4: cost-based JoinPlanning — structural cyclic-core hybrid (Freitag) + i-cost/binary-cost connected-subset DP (Graphflow) + AGM guard, retiring the fixed wcoj_cutover==4"
---

# Cost-Based JoinPlanning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).
**SotA (state-of-the-art) reference:** [`docs/research/optimizer-sota.md`](../research/optimizer-sota.md) Part B — read it before task 1; it is the "why these algorithms" behind the hybrid decision, cost model, and ordering.

> **Executed 2026-09-04 (HDB-46).** The banner that stood here listed six
> blockers; how each was closed is in *Execution notes and deviations* at the
> end. The design text below is the one that was built, with three deviations
> noted there.

**Goal:** Make `JoinPlanning` the single cost-based search stage, producing a per-BGP
`JoinSpec` that (a) routes **acyclic parts to binary hash joins and cyclic cores to leapfrog
triejoin** on a *structural* basis, (b) orders within each part by a **unified additive
i-cost + binary-cost model**, and (c) retires the fixed `wcoj_cutover == 4`. This is the
Graphflow optimizer (i-cost + connected-subset dynamic programming, DP) wrapped in the
Freitag structural hybrid, with an AGM upper-bound guard.

## Architecture (SPEC-23 §5.5; `docs/research/optimizer-sota.md` Part B)

The current global switch — `≤3 patterns → binary-hash`, `≥4 → WCOJ`, variable order by
descending degree, `_est` ignored — is **structurally wrong**: a 6-pattern acyclic star
should stay binary; a 3-pattern triangle should go WCOJ. Replace it in three layers.

**(1) Structural hybrid decision (Freitag, VLDB 2020) — the first-order routing.** Build the
BGP's **variable-connection graph**, decompose into **acyclic tree parts** and **cyclic
cores**. Route tree parts → binary hash join (which HornDB already does well); route cyclic
cores → leapfrog triejoin (which avoids the large intermediate a binary plan would
materialize on a cycle). WCOJ is used **only for the sub-plans that need it**, embedded as
multi-way nodes in an otherwise-binary tree. HornDB's runtime **already builds the hash-trie
on demand** — no persistent per-order index required, exactly Freitag's model. Cheap to
compute; a strict correctness improvement over the pattern-count switch.

**(2) Unified cost model = binary-join cost + i-cost (Graphflow, VLDB 2019) — the search
metric.** One additive scale (`cost = card(result) + cost(children)`) where a WCOJ multi-way
intersection step is charged **i-cost** (the total size of the adjacency/columnar runs read
and intersected to extend the match by one variable) and a binary step pays the usual
build+probe. Because both are on one additive scale, a WCOJ extension and a binary join are
compared **on equal footing** — the WCOJ-vs-hash choice becomes a genuine cost comparison,
not a heuristic. Fed by the predicate-keyed catalogue from [PLAN-23-03] (HornDB's advantage
over Graphflow: RDF predicates make a compact, natural catalogue). ⚠ verify i-cost constants
against the Graphflow PDF before coding.

**Unified-memory materialization term.** HornDB targets HBM/CXL, so the cost model carries a
**materialization term** — trie-materialization vs hash-table build is memory-bandwidth-bound,
not the CPU-bound pairwise model DuckDB assumes (SPEC-23 §5.5). This term is what makes the
cyclic-core-vs-binary boundary bench-calibrated rather than assumed.

**(3) Search: connected-subset DP, greedy fallback.** DP over *connected* subsets of BGP
variables (the classic DPccp join-ordering algorithm, restricted to connected subqueries):
for each connected subset keep the
cheapest plan; extend by a binary join **or** a WCOJ multi-way intersection; cost via the
unified metric. The DP is exponential in query-graph size, so above an explicit relation-count
threshold + work budget (DuckDB's dual scaling guard) fall back to **greedy operator
ordering** — most SPARQL BGPs are small, so DP usually applies. Hash build-side is chosen in a
*late* pass (DuckDB `BuildProbeSideOptimizer`), keeping the DP state small.

**Variable ordering within a WCOJ core.** Any order is worst-case optimal (generic join), so
ordering is a cost/constant-factor problem — let the i-cost drive it: greedy
**smallest-estimated-multiway-intersection first**, falling back to **min-degree** when
estimates are absent, seeded by today's descending-degree tie-break. Full GHD (generalized
hypertree decomposition) / fractional-hypertree-width decomposition (EmptyHeaded) and
adaptive runtime reordering (ADOPT) are
design-for/later (PLAN-23-05, `docs/research/optimizer-sota.md` Part B).

**AGM upper-bound guard.** Compute the tiny fractional-edge-cover linear program (LP) per candidate WCOJ core
(microseconds — arity ≤ 3, few constraints; closed form for paths/stars/triangles/cliques) as
a cheap sanity bound and tie-breaker: a cyclic core whose AGM bound is small relative to its
input product is a strong WCOJ signal. Apply the §8 #2 calibration decision (raw bound vs
empirical correction).

**Free Join is the horizon, not this phase.** The structural hybrid gives a hard binary/WCOJ
split per subplan; Free Join's GHT (Generalized Hash Trie) + granularity-per-relation knob
(SIGMOD 2023) unifies them into one continuum and COLT (column-oriented lazy tries) fixes
WCOJ's cache behaviour on acyclic queries. Keep the
`JoinSpec` IR from *foreclosing* that continuum (don't hardcode a binary switch), but the
GHT/COLT executor rewrite is PLAN-23-05's design-for item, gated on measured evidence.

**Tech stack / crates touched:** `horndb-wcoj` (`crates/wcoj/src/planner.rs` + `plan.rs` —
`Planner::choose`/`ExecutionPlan` grow real bodies: cyclic-core decomposition, i-cost-costed
connected-subset DP, greedy fallback, AGM guard, per-subplan mode); `horndb-sparql`
(`crates/sparql/src/plan/` — the `JoinPlanning` `LogicalPass` calls into the WCOJ planner for
the per-BGP `JoinSpec`, keeping the `sparql → wcoj` dep direction; hash DP/greedy for non-BGP
algebra joins; late build-side pass).

**Depends on:**
- [PLAN-23-03] — the layered `Stats` seam + CS/degree-bound estimator (hard prerequisite; it *is* the cost signal).
- [PLAN-23-01] / [PLAN-23-02] — framework scaffolding + heuristic passes (the pass registry `JoinPlanning` slots into).
- **SPEC-03** — WCOJ acceptance shapes (4-cycle, WatDiv/LUBM subset) and the differential fuzzer oracle.
- SPEC-23 §5.5, §8 open questions #2 and #3; `docs/research/optimizer-sota.md` Part B.

**Prerequisites to unblock:**
- [x] [PLAN-23-03] executed: real `Cardinality` (CS + degree bounds) over `Stats` available to the planner.
- [x] `ExecutionPlan` refactored to represent per-subplan mode (binary tree with embedded multi-way WCOJ nodes) — `JoinSpec` in `plan.rs`.
- [x] §8 #2 answered: raw AGM bound, used only as a cap on the cardinality estimate (no calibration needed — it is never a cost term).
- [x] §8 #3 answered: `wcoj` consumes `&dyn Stats` directly; `Executor::for_bgp(source, bgp, planner, &dyn Stats, cancel)`.
- [x] Materialization term: one uncalibrated constant per node (`cost::MATERIALIZE_WEIGHT`, with `HASH_BUILD_WEIGHT` for hash builds); a hornbench calibration is a follow-up, not a blocker (see deviations).
- [x] The WCOJ differential oracle is green and now also runs the cost-based plan and a hand-built hybrid plan against the oracle on every case.

## Task outline

1. [x] **Per-subplan `ExecutionPlan` refactor.** Change the plan IR from one-mode-per-BGP to a tree
   of binary joins with embedded multi-way WCOJ nodes. No behavior change yet (still driven by
   the old heuristic); golden-plan snapshots hold. This unblocks everything below.
2. [x] **Structural cyclic-core decomposition (Freitag).** Build the variable-connection graph;
   split the BGP into acyclic tree parts (→ binary) and cyclic cores (→ WCOJ). Replace the
   `len >= cutover` global switch with this structural routing. Unit-test: 6-pattern star stays
   binary; 3-pattern triangle and the 4-cycle go WCOJ.
3. [x] **i-cost + binary-cost unified model (Graphflow).** Implement the additive cost over a hybrid
   plan: i-cost for multi-way extensions (from the [PLAN-23-03] predicate catalogue), build+probe
   for binary, plus the unified-memory materialization term. ⚠ verify i-cost constants vs the PDF.
4. [x] **Connected-subset DP + greedy fallback.** DP over connected variable subsets choosing per
   step between binary join and WCOJ extension and the order jointly; greedy operator ordering
   past the relation-count threshold + work budget. Emit the `JoinSpec`.
5. [x] **Greedy WCOJ variable ordering.** Within a WCOJ core, order variables by
   smallest-estimated-multiway-intersection first (min-degree fallback), seeded by descending
   degree. Emit the variable elimination order in the `JoinSpec`.
6. [x] **AGM guard.** Compute the fractional-edge-cover LP (or closed form) per candidate WCOJ core
   as an upper-bound tie-breaker; apply the §8 #2 calibration. Unit-test the bound against the
   triangle / 4-cycle worst cases.
7. [x] **Late build-side pass.** A separate pass picks the hash build side (DuckDB
   `BuildProbeSideOptimizer`), keeping the ordering search state small.
8. [x] **Result-parity + ordering-win harness.** Verify **zero** result-set changes vs the WCOJ
   differential oracle across the acceptance shapes; measure the ordering win on the 4-cycle +
   WatDiv/LUBM subset (on `hornbench`); document at least one BGP shape where the structural /
   cost-based planner beats the fixed cutover (§7.5). Keep the fixed cutover reachable as a
   pragma for bisection.

## Open questions (carried from SPEC-23 §8, plus SotA-specific)

- **#2 AGM cost calibration** — is the fractional-edge-cover upper bound tight enough on HornDB
  workloads, or does it need an empirical correction to sit on-scale with the i-cost/binary-cost
  terms?
- **#3 sparql/wcoj API boundary** — where the WCOJ planner ends and the SPARQL planner begins;
  what data crosses the crate seam.
- **i-cost constants** — Graphflow's exact i-cost formula/constants were not extractable from the
  abstract (egress blocked the PDF); verify before task 3 hardens.
- **Cyclic-core granularity** — Freitag routes whole cyclic cores to WCOJ; Free Join would split
  finer. Where the structural split stops and per-relation granularity begins is the boundary
  between this phase and PLAN-23-05's Free-Join horizon.

## Acceptance criteria (SPEC-23 §7.4, §7.5)

- On the SPEC-03 acceptance shapes (4-cycle + WatDiv/LUBM subset) the cost-based planner
  **matches or beats** the descending-degree / fixed-cutover heuristic on the harness, with
  **zero** result-set changes vs the WCOJ differential oracle, and no query regressing beyond a
  set tolerance.
- **At least one BGP shape** exists where the structural / cost-based planner correctly picks a
  plan the fixed `wcoj_cutover == 4` rule got wrong (e.g. the 6-pattern acyclic star it wrongly
  sends to WCOJ, or a 3-pattern triangle it wrongly keeps binary), documented in the harness.

## Execution notes and deviations (HDB-46, 2026-09-04)

What was built, per task, and where the code diverges from the outline above.

- **Task 1 — `JoinSpec`** (`crates/wcoj/src/plan.rs`): `Scan { pattern }`,
  `HashJoin { build, probe }`, `Wcoj { patterns, var_order }`. `ExecutionPlan`
  (one mode per BGP) is kept only as the legacy path
  (`ExecutionPlan::for_bgp(bgp, cutover)` → `JoinSpec::from_execution_plan`),
  reachable through `HORNDB_WCOJ_CUTOVER=<n>` for bisection.
- **Task 2 — cyclic core** (`cost.rs::cyclic_core`): GYO ear removal. The
  core is never split by a hash join (`Search::respects_core`).
- **Tasks 3, 5, 6 — cost model** (`cost.rs`): i-cost per variable extension
  from per-predicate counts and NDVs; hash join = weighted build + probe +
  output; sub-nodes of a hybrid tree add a materialisation term (the
  whole-BGP leapfrog streams and pays none); connected degree-first variable
  order with a first-variable shortlist sweep; AGM bound as a cap on `card`,
  computed only for ≤ 5 patterns. See *Rework after review* below.
- **Task 4 — search** (`planner.rs`): DP over connected, core-respecting
  pattern subsets (one core per connected component); each subset is costed
  as one WCOJ node and as every hash-join split of two solved halves.
  `MAX_DP_PATTERNS = 5`; past it, greedy build-up over units (cores plus
  single patterns). A hybrid tree is taken only if it beats whole-BGP WCOJ by
  `HYBRID_MARGIN` (2×).
- **Task 7 — build side** (`planner.rs::assign_build_sides`): late pass,
  smaller estimated side builds.
- **Task 8 — parity**: `tests/differential_fuzz.rs` runs the cost-based plan
  and a hand-built hybrid `JoinSpec` against the binary-hash oracle on every
  case; `tests/planner_choice.rs` pins the structural rules, the AGM bound, and
  the HDB-108 q3 ordering. Hornbench numbers pending (runner offline at PR
  time) — `docs/benchmarks.md`.

Deviations:

1. **The binary-hash executor is a materialising evaluator, not a streaming
   production path.** The outline assumed "binary hash join, which HornDB
   already does well". On `main` the binary-hash executor was the Stage-1
   reference oracle: it scans every pattern in full and materialises every
   intermediate. It now walks each pattern's preferred ordering (so bound
   positions seek instead of scanning) and evaluates a `JoinSpec` tree, but it
   still materialises each node. The cost model is honest about that (a
   materialisation term per node), so with today's executors it rarely
   prefers a hash join over the streaming leapfrog for connected BGPs; the
   plan shape is what changed most, not the executor choice. A streaming hash
   join, or the Free Join continuum, is what would let the binary side win —
   PLAN-23-05 territory.
2. **The SPARQL `JoinPlanning` pass is not registered.** Planning happens at
   execution time in `HornBackend`, which hands its cached per-scope
   `SnapshotStats` to `Executor::for_bgp`. A logical pass would need the
   `Stats` object at plan time and a way to carry the `JoinSpec` into the
   physical plan; neither existed and neither is needed for the per-BGP win.
   Algebra-level (non-BGP) join ordering and EXPLAIN display of the `JoinSpec`
   are follow-ups.
3. **Ordering-win harness item and materialisation-term calibration are
   deferred to hornbench.** The bench host was offline; the parity gate ran
   locally (fuzzer, sparql suite, conformance harness). The constants
   `HASH_BUILD_WEIGHT` / `MATERIALIZE_WEIGHT` are uncalibrated
   (`ponytail:` comment in `cost.rs`).

### Rework after review (PR #342)

The first cut planned well on paper and lost badly on the executor. Same-process
A/B (`crates/wcoj/tests/plan_ab.rs`: planned spec vs whole-BGP WCOJ in degree
order, distinct predicates per pattern, 20k subjects, release build, best of 3;
**laptop, not recorded in `docs/benchmarks.md`**):

| shape | degree order | planned, before rework | planned, after rework |
|---|---|---|---|
| triangle | 20.3 ms | 1936.4 ms (×95.6) | 19.3 ms (×1.00, WCOJ `[0,2,1]`) |
| 4-cycle | 47.9 ms | 5634.4 ms (×117.6) | 46.1 ms (×1.00) |
| 4-path | 64.1 ms | 121.6 ms (×1.90, hash tree) | 61.9 ms (×0.98, WCOJ) |
| 4-star | 23.9 ms | 74.9 ms (×3.14, hash tree) | 25.7 ms (×1.00, WCOJ) |
| 5-star | 37.5 ms | 92.0 ms (×2.45, hash tree) | 37.5 ms (×1.00, WCOJ) |
| star + tail | 37.2 ms | 83.0 ms (×2.23, hash tree) | 36.5 ms (×0.99, WCOJ) |
| knows 2-path + type | 0.4 ms | 0.4 ms (×0.97) | 0.4 ms (×1.00) |
| HDB-108 q3 (7 patterns) | 21.9 ms | 7.3 ms (×0.33, hash tree) | 2.1 ms (×0.10, WCOJ `?customer` first) |

Planning time: q3 1607 µs → 178 µs; 10-pattern star 96 ms → 114 µs
(`tests/planner_choice.rs::ten_pattern_star_plans_fast`, bound 200 µs release).
The "degree order" column moves a little between runs; the two planned columns
come from separate runs against their own degree baseline.

What changed, per review item:

1. **Executor cliff, not a planning error (root cause of the ×95/×117).**
   `try_arm_simd` (`executor/wcoj.rs`, `trie/leapfrog.rs`) asked the iterators
   for `active_run` in pattern-index order. `VecIter::active_run` eagerly
   builds a deduplicated copy of its whole level range (a ~30k-row predicate
   block) before the partner is even checked, and the copy is discarded on
   every `reset`/`up`. Any order where the wide iterator had the lower
   pattern index paid that copy per outer binding. Fix: a cheap
   `active_run_ready(depth)` probe on `OrderedTripleIter`/`TrieIterator`
   (default `false`; `VecIter` checks the run length), and both sides must
   answer yes before either run is built. Every WCOJ order is now within noise
   of degree order on the SPEC-03 shapes (`plan_ab.rs`).
2. **Variable order** (`cost.rs::OrderSearch`): candidates are the unbound
   variables that share a pattern with the bound prefix; rank by
   (degree desc, log10 bucket of the running row count, step cost). The
   first variable is chosen by a shortlist sweep: run the search from the
   degree-first start and from the two most selective starts, keep the
   cheapest. That sweep is what keeps the q3 win (`?customer` before
   `?order`, 22 ms → 2.1 ms) that a pure degree-first order loses.
3. **Hybrid vs whole-WCOJ**: only sub-nodes of a hash tree pay
   `MATERIALIZE_WEIGHT` (8) per row; `HASH_BUILD_WEIGHT` is 8; the tree must
   beat whole-BGP WCOJ by `HYBRID_MARGIN = 2` or the planner returns one WCOJ
   node. Acyclic 4-5-pattern shapes now stay on the streaming leapfrog.
4. **Planning time**: `StatsEstimator::estimate_bgp_fast` (product of
   per-pattern bounds, no characteristic-set walk) for multi-pattern
   cardinalities; per-mask memo for `card` and for the WCOJ cost; AGM only
   for ≤ 5 patterns (`AGM_MAX_PATTERNS`); `MAX_DP_PATTERNS = 5` and
   `DP_WORK_BUDGET` removed (it never tripped — the pattern cap is the guard);
   greedy attaches units by O(k) estimates.
5. **Stats off the query path** (`horn.rs::planning_stats`): the first BGP
   on a scope spawns the `horndb-stats` thread and plans on `ZeroStats`
   (structural order) until the summary lands; later queries hit the cache.
   Residual cold-start cost: queries during the build window use the
   structural plan; a write during the window cannot merge in place and
   triggers a fresh build; a full cache (`STATS_CACHE_MAX_SCOPES = 8`) starts
   no build. `snapshot_stats` (sync) stays for EXPLAIN. A single-pattern BGP
   always gets the structural order, so its plan never depends on whether
   statistics are ready.
6. **Fuzzer** compares sorted multisets (duplicate rows are a mismatch) and
   honours `PROPTEST_CASES`.
7. **Cores per connected component** (`planner.rs::components`): two
   vertex-disjoint triangles are two cores; a hash join may split between
   them, never inside one (`tests/planner_choice.rs::cores_are_per_connected_component`).
8. **Empty BGP is the join identity** again: one solution with no bindings
   (`empty_bgp_is_the_join_identity`).

Parity after the rework: WCOJ differential fuzzer at `PROPTEST_CASES=2000`,
`horndb-wcoj` + `horndb-sparql` (server) test suites, harness sparql11-eval
unchanged (374 pass / 173 skip / 0 fail), trainmarks q1-q6 on `medium`
identical between the cost-based planner and `HORNDB_WCOJ_CUTOVER=4` except
q2's `SUM(total_spend)` last digits, which follow f64 summation order and
change with any row-order change.
