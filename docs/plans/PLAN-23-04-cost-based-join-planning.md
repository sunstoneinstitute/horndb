---
status: draft
date: 2026-07-07
scope: "Phase 4: cost-based JoinPlanning — the one search stage — bimodal WCOJ variable-order (AGM bound) + hash DP/greedy, retiring the fixed wcoj_cutover==4"
---

# Cost-Based JoinPlanning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **skeleton** (task outline only), not an executable TDD plan.
>
> **Blocking dependencies:**
> - **[PLAN-23-03] (statistics seam + estimator) must land first.** `JoinPlanning` searches over cost, and cost is cardinality-dominated — it needs the `Stats`-backed `Cardinality` estimator. Without it there is nothing to cost.
> - **SPEC-23 §8 open question #2 "AGM cost calibration"** — the fractional-edge-cover bound is an *upper* bound, not an expected size; how loose it is on HornDB workloads, and whether it needs an empirical correction to be comparable with the hash-tree cost on one scale, must be answered before the WCOJ-vs-hash choice can be trusted.
> - **SPEC-23 §8 open question #3 "WCOJ-planner / SPARQL-planner API boundary"** — the exact API between `horndb-sparql`'s `JoinPlanning` pass and `horndb-wcoj`'s per-BGP planner (does `wcoj` see `Stats` directly or a digested estimate?) must be pinned; it shapes where the search code lives.

**Goal:** Make `JoinPlanning` the single cost-based search stage — producing a per-BGP `JoinSpec` via a bimodal search (WCOJ variable-elimination order costed by an AGM bound; binary-hash tree costed by DuckDB-style DP/greedy) — and make the WCOJ-vs-hash choice cost-based, retiring the fixed `wcoj_cutover == 4`.

**Architecture (SPEC-23 §5.5):** `JoinPlanning` is the **only** pass that searches, costed on one additive, cardinality-dominated scale (`cost = card(result) + cost(children)`), extended to be hybrid-aware. It is bimodal: (a) a **WCOJ sub-plan**'s plan object is a **variable elimination order** — retarget `sparopt`'s greedy connected seed-and-grow from "pick next pattern" to "pick next variable" (most-constrained / smallest-NDV-first, seeded by today's descending-degree tie-break), cost = AGM / fractional-edge-cover bound, *not* a product of pairwise selectivities; (b) a **binary-hash sub-plan** (low-arity BGPs, non-BGP algebra joins) uses DuckDB-style DP over the connected query subgraph for small relation counts and **greedy operator ordering past an explicit threshold + work budget**, with hash build-side chosen in a *late* pass. The WCOJ-vs-hash choice itself becomes cost-based, retiring the fixed `wcoj_cutover == 4` in `crates/wcoj/src/planner.rs` (`Planner::choose` currently ignores `_est`; `plan.rs::for_bgp` uses descending-degree var order with `len >= cutover`). Because HornDB targets unified memory, the cost model must carry a **materialization term** (trie-materialization vs hash-table is memory-bandwidth-bound), not DuckDB's CPU-bound pairwise model.

**Tech stack / crates touched:** `horndb-wcoj` (`crates/wcoj/src/planner.rs` — `Planner::choose` and `ExecutionPlan::for_bgp` grow their real bodies: AGM-costed variable-order search, cost-based WCOJ-vs-hash); `horndb-sparql` (`crates/sparql/src/plan/` — the `JoinPlanning` `LogicalPass` calls into the WCOJ planner for the per-BGP `JoinSpec`, keeping the `sparql → wcoj` dep direction; hash DP/greedy for algebra joins; late build-side pass à la DuckDB `BuildProbeSideOptimizer`).

**Depends on:**
- [PLAN-23-03] — the `Stats` seam + NDV/counts estimator (hard prerequisite).
- [PLAN-23-01] / [PLAN-23-02] — framework scaffolding + heuristic passes (the pass registry `JoinPlanning` slots into).
- **SPEC-03** — WCOJ acceptance shapes (4-cycle, WatDiv/LUBM subset) and the differential fuzzer oracle.
- SPEC-23 §5.5, §8 open questions #2 and #3.

**Prerequisites to unblock:**
- [ ] [PLAN-23-03] executed: real `Cardinality` over `Stats` available to the planner.
- [ ] §8 #2 answered: AGM bound calibration model decided (raw bound vs empirical correction) and how it compares on-scale to hash-tree cost.
- [ ] §8 #3 answered: the `sparql ↔ wcoj` planner API frozen (what crosses the crate boundary).
- [ ] A materialization-cost term for unified memory is characterized well enough to weigh trie-materialization against hash-table build (a bench, likely on `hornbench`).
- [ ] The WCOJ differential oracle is confirmed green as the result-parity gate before any ordering change ships.

**Task outline:**
1. **WCOJ variable-order search.** In `crates/wcoj/src/planner.rs`/`plan.rs`, replace the descending-degree `for_bgp` var order with a greedy connected seed-and-grow over *variables* (smallest-NDV / most-constrained first), keeping descending-degree as the tie-break seed. Emit a variable elimination order as the `JoinSpec` plan object.
2. **AGM / fractional-edge-cover cost.** Cost the WCOJ sub-plan by the AGM bound rather than pairwise selectivity products. Apply the §8 #2 calibration decision. Unit-test the bound against known BGP shapes (e.g. the triangle/4-cycle worst case).
3. **Hash-tree DP/greedy search.** For low-arity BGPs and non-BGP algebra joins, DP over the connected subgraph for small relation counts, greedy past an explicit threshold + ~work-budget (DuckDB's dual scaling guard). Produce a binary join tree `JoinSpec`.
4. **Cost-based WCOJ-vs-hash choice.** Replace `wcoj_cutover == 4` with a cost comparison on the one additive scale, including the unified-memory materialization term. Keep the fixed cutover reachable as a fallback/pragma for bisection.
5. **Late build-side pass.** A separate pass picks the hash build side (DuckDB `BuildProbeSideOptimizer`), keeping the ordering search state small.
6. **Result-parity + ordering-win harness.** Verify zero result-set changes vs the WCOJ differential oracle across the acceptance shapes; measure the ordering win on the 4-cycle + WatDiv/LUBM subset (on `hornbench`); document at least one BGP shape where the cost model beats the fixed cutover (§7.5).

**Open questions (carried from SPEC-23 §8):**
- **#2 AGM cost calibration** — is the fractional-edge-cover upper bound tight enough on HornDB workloads, or does it need a learned/empirical correction to sit on-scale with hash-tree cost?
- **#3 sparql/wcoj API boundary** — where the WCOJ planner ends and the SPARQL planner begins; what data crosses the crate seam.

**Acceptance criteria (SPEC-23 §7.4, §7.5):**
- On the SPEC-03 acceptance shapes (4-cycle + WatDiv/LUBM subset) the cost-based planner **matches or beats** the descending-degree / fixed-cutover heuristic on the harness, with **zero** result-set changes vs the WCOJ differential oracle, and no query regressing beyond a set tolerance.
- **At least one BGP shape** exists where the cost model correctly picks a plan the fixed `wcoj_cutover == 4` rule got wrong, documented in the harness.
