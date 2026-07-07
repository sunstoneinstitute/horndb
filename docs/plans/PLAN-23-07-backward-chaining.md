---
status: draft
date: 2026-07-07
scope: "Phase 7: backward-chaining — magic-sets/demand transformation + SLG tabling generate query-driven rewrites so a query answers without full materialization; the hybrid forward/backward core bet (ADR-0005) becomes real"
---

# Backward-Chaining Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **skeleton** (task outline only), not an executable TDD plan. This is the deepest phase — it depends on subsystems that do not exist and on an unresolved research question.
>
> **Blocking dependencies:**
> - **[PLAN-23-06] (reasoning in the IR) must land first** — backward-chaining generates the query-driven rewrites that populate the reasoning rewrite passes + delegate nodes and the materialization catalog seam built there.
> - **SPEC-03 F4 (magic-sets / demand transformation) does not exist.** This is the transformation that turns a query goal into demand-restricted rules.
> - **SPEC-03 F5 (SLG tabling) does not exist.** This is the tabled resolution that answers recursive goals with termination.
> - **SPARQL backward-chained entailment mode (SPEC-07) does not exist.** This is the user-facing surface that requests query-driven (rather than fully-materialized) answering.
> - **SPEC-23 §8 open question #4 "recursive-fixpoint costing" BLOCKS this phase** — termination/cardinality/cost for fixpoint nodes on the additive scale is unresolved; much may have to be delegated opaque to the closure operator rather than costed.

**Goal:** Answer a query without full materialization by generating query-driven rewrites (magic-sets/demand transformation + SLG tabling) and making the per-subgoal materialize-vs-rewrite-vs-delegate choice real — turning the hybrid forward/backward core bet (ADR-0005) into working code.

**Architecture (SPEC-23 §5.8 machinery + §6 phase 7):** Magic-sets / demand transformation (SPEC-03 F4) rewrites the query's rules so only demand-relevant facts are derived; SLG tabling (SPEC-03 F5) resolves recursive goals with proper termination. Together they generate the query-driven rewrites that flow into the [PLAN-23-06] reasoning IR, so a query answers without full up-front materialization, and the engine makes a genuine **per-subgoal materialize-vs-rewrite-vs-delegate** choice. Fixpoint nodes carry termination/cardinality/cost concerns that the AGM/hash cost model was not built for — much of which may be **delegated opaque** to the closure operator (SPEC-05) rather than costed. SPARQL backward-chained entailment mode (SPEC-07) is the surface that requests this path. This is where ADR-0005's hybrid forward/backward bet becomes real: materialize/delegate the closure subset, backward-chain the rest.

**Tech stack / crates touched:** `horndb-wcoj` (SPEC-03 F4 magic-sets/demand transformation, F5 SLG tabling); `horndb-owlrl` (SPEC-04 — the rules the demand transformation restricts); `horndb-closure` (SPEC-05 — opaque fixpoint delegate for recursive subgoals); `horndb-sparql` (SPEC-07 — backward-chained entailment mode surface; per-subgoal strategy selection in the reasoning IR from [PLAN-23-06]); `horndb-incremental` (SPEC-06 — what is already materialized, feeding the per-subgoal choice).

**Depends on:**
- [PLAN-23-06] — reasoning in the IR (the rewrite passes / delegate nodes / catalog seam this phase feeds).
- [PLAN-23-01]..[PLAN-23-05] — the full optimizer framework beneath it.
- **SPEC-03 F4** (magic-sets/demand transformation) and **F5** (SLG tabling) — must be built.
- **SPEC-07** — backward-chained entailment mode.
- **ADR-0005** — the hybrid forward/backward core bet this phase realizes.
- SPEC-23 §5.8, §6 phase 7, §8 open question #4.

**Prerequisites to unblock:**
- [ ] SPEC-03 F4 (magic-sets/demand transformation) specified and implemented.
- [ ] SPEC-03 F5 (SLG tabling) specified and implemented, with a termination guarantee.
- [ ] SPEC-07 backward-chained entailment mode specified (how a query opts into query-driven answering).
- [ ] SPEC-23 §8 #4 resolved: termination/cardinality/cost model for fixpoint nodes, and the costed-vs-delegated-opaque boundary.
- [ ] [PLAN-23-06] executed: reasoning rewrite passes, delegate nodes, and the materialization catalog seam exist.
- [ ] A parity oracle (full-materialization answers) exists to validate backward-chained results.

**Task outline:**
1. **Magic-sets / demand transformation (SPEC-03 F4).** In `horndb-wcoj`, transform the query goal + rules into demand-restricted rules so only demand-relevant facts are derived. Feed the result into the [PLAN-23-06] reasoning rewrite passes.
2. **SLG tabling (SPEC-03 F5).** Tabled resolution of recursive goals with proper termination (memoized answer/goal tables). This is the recursion engine the fixpoint delegate nodes rely on.
3. **SPARQL backward-chained entailment mode (SPEC-07).** The surface: a query opts into query-driven answering instead of reading the fully-materialized closure. Wire it to the reasoning IR.
4. **Per-subgoal materialize-vs-rewrite-vs-delegate.** Extend the [PLAN-23-06] catalog-seam cost comparison to decide, per subgoal, whether to read materialized facts, backward-chain a rewrite, or delegate to the SPEC-05 closure operator. Fixpoint subgoals likely delegate opaque per §8 #4.
5. **Fixpoint cost/termination handling.** Apply the §8 #4 resolution: cost/cardinality/termination for fixpoint nodes on the additive scale, delegating opaquely where costing is intractable.
6. **Hybrid parity + coverage harness.** Prove backward-chained answers match the full-materialization oracle across the conformance subset, that recursive goals terminate, and that the per-subgoal strategy split is genuinely cost-driven (ADR-0005 hybrid goal demonstrated end-to-end).

**Open questions (carried from SPEC-23 §8):**
- **#4 recursive-fixpoint costing** — the blocking one: how termination, cardinality, and cost for fixpoint nodes fit an additive scale built for non-recursive AGM/hash costing; how much must be delegated opaque to the closure operator.
- (from §5.8) the exact division of labor between magic-sets rewrites, SLG tabling, and GraphBLAS delegation for a given recursive subgoal — a boundary to pin during F4/F5 design.

**Acceptance criteria (derived from ADR-0005 hybrid goal + SPEC-23 §8 #4 resolution):**
- **Hybrid answering works:** a representative query is answered **without full materialization**, with results identical to the full-materialization oracle.
- **Termination:** recursive/fixpoint subgoals (transitive closure, rule fixpoints) terminate under SLG tabling.
- **Cost-driven per-subgoal split:** the materialize-vs-rewrite-vs-delegate choice is made per subgoal on the §5.5 additive scale (per §8 #4's resolution), with documented cases exercising each strategy.
- **Result parity + no regression (§7.1/§7.4 carried forward):** the conformance subset and the differential oracle stay green with backward-chaining enabled.
