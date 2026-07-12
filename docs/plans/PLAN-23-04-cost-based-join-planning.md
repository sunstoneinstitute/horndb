---
status: draft
date: 2026-07-07
scope: "Phase 4: cost-based JoinPlanning — structural cyclic-core hybrid (Freitag) + i-cost/binary-cost connected-subset DP (Graphflow) + AGM guard, retiring the fixed wcoj_cutover==4"
---

# Cost-Based JoinPlanning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).
**SotA (state-of-the-art) reference:** [`docs/research/optimizer-sota.md`](../research/optimizer-sota.md) Part B — read it before task 1; it is the "why these algorithms" behind the hybrid decision, cost model, and ordering.

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **design + task outline**, not an executable TDD (test-driven development) plan. The *algorithm* choices below are firm (SotA-grounded); the *code* waits on the blockers.
>
> **Blocking dependencies:**
> - **[PLAN-23-03] (statistics seam + estimator) must land first.** `JoinPlanning` searches over cost, and the i-cost model is cardinality-dominated — it needs the `Stats`-backed estimator (Characteristic Sets + degree bounds). Without it there is nothing to cost.
> - **SPEC-23 §8 open question #2 "AGM cost calibration"** — the AGM (fractional-edge-cover) bound is a proven *upper* bound on join output size, not an expected size; how loose it is on HornDB workloads, and whether it needs an empirical correction to sit on-scale with the i-cost/binary-cost terms, must be answered before the WCOJ-vs-hash choice can be trusted.
> - **SPEC-23 §8 open question #3 "WCOJ-planner / SPARQL-planner API boundary"** — the exact API between `horndb-sparql`'s `JoinPlanning` pass and `horndb-wcoj`'s per-BGP planner must be pinned; it shapes where the search code lives.
> - **`ExecutionPlan` must become per-subplan.** Today it picks *one* mode (`Wcoj` or `BinaryHash`) for the whole BGP (Basic Graph Pattern — the block of triple patterns a SPARQL query joins). The structural hybrid (below) embeds multi-way WCOJ (worst-case optimal join) nodes inside an otherwise-binary plan, so the plan IR must represent a **tree of binary joins with WCOJ super-operators at the cyclic cores** — a prerequisite refactor.

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
- [ ] [PLAN-23-03] executed: real `Cardinality` (CS + degree bounds) over `Stats` available to the planner.
- [ ] `ExecutionPlan` refactored to represent per-subplan mode (binary tree with embedded multi-way WCOJ nodes).
- [ ] §8 #2 answered: AGM calibration model decided (raw bound vs empirical correction) and how it sits on-scale with i-cost.
- [ ] §8 #3 answered: the `sparql ↔ wcoj` planner API frozen (what crosses the crate boundary).
- [ ] The unified-memory materialization-cost term is characterized well enough to weigh trie-materialization against hash-table build (a bench, likely on `hornbench`).
- [ ] The WCOJ differential oracle is confirmed green as the result-parity gate before any ordering change ships.

## Task outline

1. **Per-subplan `ExecutionPlan` refactor.** Change the plan IR from one-mode-per-BGP to a tree
   of binary joins with embedded multi-way WCOJ nodes. No behavior change yet (still driven by
   the old heuristic); golden-plan snapshots hold. This unblocks everything below.
2. **Structural cyclic-core decomposition (Freitag).** Build the variable-connection graph;
   split the BGP into acyclic tree parts (→ binary) and cyclic cores (→ WCOJ). Replace the
   `len >= cutover` global switch with this structural routing. Unit-test: 6-pattern star stays
   binary; 3-pattern triangle and the 4-cycle go WCOJ.
3. **i-cost + binary-cost unified model (Graphflow).** Implement the additive cost over a hybrid
   plan: i-cost for multi-way extensions (from the [PLAN-23-03] predicate catalogue), build+probe
   for binary, plus the unified-memory materialization term. ⚠ verify i-cost constants vs the PDF.
4. **Connected-subset DP + greedy fallback.** DP over connected variable subsets choosing per
   step between binary join and WCOJ extension and the order jointly; greedy operator ordering
   past the relation-count threshold + work budget. Emit the `JoinSpec`.
5. **Greedy WCOJ variable ordering.** Within a WCOJ core, order variables by
   smallest-estimated-multiway-intersection first (min-degree fallback), seeded by descending
   degree. Emit the variable elimination order in the `JoinSpec`.
6. **AGM guard.** Compute the fractional-edge-cover LP (or closed form) per candidate WCOJ core
   as an upper-bound tie-breaker; apply the §8 #2 calibration. Unit-test the bound against the
   triangle / 4-cycle worst cases.
7. **Late build-side pass.** A separate pass picks the hash build side (DuckDB
   `BuildProbeSideOptimizer`), keeping the ordering search state small.
8. **Result-parity + ordering-win harness.** Verify **zero** result-set changes vs the WCOJ
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
