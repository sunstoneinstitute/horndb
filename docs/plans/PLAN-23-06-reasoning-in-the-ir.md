---
status: draft
date: 2026-07-07
scope: "Phase 6: reasoning enters the logical IR as first-class rewrite passes + delegate nodes, with a reasoning/materialization catalog seam making materialize-vs-rewrite-vs-delegate cost-based"
---

# Reasoning in the IR Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **skeleton** (task outline only), not an executable TDD plan. Phase 6 also has an open research question (below) with no answer yet.
>
> **Blocking dependencies:**
> - **[PLAN-23-01]..[PLAN-23-04] — the whole optimizer framework — must land first.** Reasoning enters the *same* logical IR and pass registry, and materialize-vs-rewrite-vs-delegate is costed on the §5.5 additive scale, so the IR, pass registry, `Stats` seam, and cost-based `JoinPlanning` are all prerequisites.
> - **A demand-driven / backward-chaining capability must exist** to make materialize-vs-rewrite-vs-delegate a *real* choice. **Today HornDB fully materializes the OWL 2 RL closure up front** (SPEC-23 §5.8) and runs SPARQL as pattern matching over the closed graph — so with only forward materialization there is nothing to choose. The query-driven machinery lands in [PLAN-23-07]; this phase builds the IR surface it plugs into.
> - **SPEC-23 §8 open question #4 "recursive-fixpoint costing" BLOCKS this phase** — the cost model assumes non-recursive AGM/hash costing, but reasoning rewrites introduce recursion (transitive closure, rule fixpoints). How cost/cardinality/termination for a fixpoint node fit the additive scale — and how much must be delegated opaquely to the closure operator rather than costed — is unresolved.
> - **No prior-art borrow.** The three surveyed optimizers (Oxigraph `sparopt`, DuckDB, ClickHouse) are all *non-reasoning* engines; this layer is HornDB-specific with no borrow-from-X answer.

**Goal:** Make reasoning a first-class citizen of the logical IR — as rewrite passes and delegate nodes — so the optimizer can choose materialize-vs-rewrite-vs-delegate per pattern on the same cost scale as join ordering.

**Architecture (SPEC-23 §5.8):** Reasoning enters the §5.1 logical IR as **first-class rewrite passes** — new `PassId`s running *before* `JoinPlanning` that expand/substitute patterns from the TBox (e.g. `?x a :C` → UNION over subclasses; a transitive rule → a recursive/fixpoint pattern) — plus **delegate nodes**: heavy transitive closure hands off to the specialized GraphBLAS operator (SPEC-05) via a `ClosureScan` / the existing `PathClosure` (`Algebra::l`) node the optimizer *chooses* but does not try to out-plan with join reordering. A **reasoning/materialization catalog seam**, parallel to §5.3's `Stats`, records what is already closed plus each resolver's cost, so materialize-vs-rewrite-vs-delegate is cost-based on the §5.5 additive scale. Property-path closure is routed through the SPEC-05 GraphBLAS backend by selectivity (SPEC-07 F3 fast path). This is the hybrid (ADR-0005): materialize/delegate the closure subset, rewrite/backward-chain the rest.

**Tech stack / crates touched:** `horndb-sparql` (`crates/sparql/src/plan/` — reasoning rewrite `PassId`s in the registry; `ClosureScan` / `PathClosure` as delegate nodes; the reasoning/materialization catalog seam); `horndb-owlrl` (SPEC-04 — the TBox/rule source the rewrite passes expand from); `horndb-closure` (SPEC-05 — the GraphBLAS closure operator delegate target); `horndb-incremental` (SPEC-06 — what is already materialized/closed, feeding the catalog seam).

**Depends on:**
- [PLAN-23-01]..[PLAN-23-04] — the full optimizer framework (IR, pass registry, `Stats`, cost-based `JoinPlanning`).
- [PLAN-23-07] — backward-chaining supplies the demand-driven machinery that makes the choice real (this phase and PLAN-23-07 are co-dependent; this one builds the IR surface, that one the query-driven rewrites).
- **SPEC-04** (OWL 2 RL rules / TBox), **SPEC-05** (GraphBLAS closure delegate), **SPEC-07 F3** (property-path fast path), **ADR-0005** (hybrid forward/backward bet).
- SPEC-23 §5.8, §6 phase 6, §8 open question #4.

**Prerequisites to unblock:**
- [ ] SPEC-23 §8 #4 resolved (at least provisionally): how a fixpoint/recursive node is costed, and where the boundary between "costed" and "delegated opaque to the closure operator" sits.
- [ ] A demand-driven/backward-chaining path exists (or a stub) so materialize-vs-rewrite-vs-delegate is a real, testable choice rather than always-materialize.
- [ ] The reasoning/materialization catalog's cost model for each resolver (compiled OWL-RL rule vs GraphBLAS closure vs crosswalk/SKOS expansion) is characterized on the §5.5 additive scale.
- [ ] A full-materialization oracle exists to check result parity against rewrite/delegate plans.

**Task outline:**
1. **Reasoning/materialization catalog seam.** Define a read-only seam (parallel to `Stats`) exposing what is already closed and the cost of each resolver, sourced from SPEC-04/05/06. Stub cost values until §8 #4 is settled.
2. **Subclass/subproperty rewrite pass.** A new pre-`JoinPlanning` `PassId` expanding `?x a :C` into a UNION over subclasses (and the subproperty analog) from the TBox; the optimizer then pushes filters through the expansion and orders joins across base + inferred patterns.
3. **Transitive-rule → recursive/fixpoint rewrite.** Rewrite a transitive pattern into a recursive/fixpoint form, or mark it for delegation (task 4). Guarded by the §8 #4 costing decision — likely delegated rather than costed initially.
4. **Delegate nodes for closure.** Introduce/repurpose `ClosureScan` / `PathClosure` so heavy transitive closure hands off to the SPEC-05 GraphBLAS operator; the optimizer *chooses* the node but does not attempt to reorder joins inside it. Route property-path closure through SPEC-05 by selectivity (SPEC-07 F3 fast path).
5. **Cost-based materialize-vs-rewrite-vs-delegate.** Wire the catalog seam into `JoinPlanning`'s cost comparison so the three strategies compete on the additive scale; materialize only the closure slice the query reaches.
6. **Result-parity harness.** Prove rewrite/delegate plans return results identical to the full-materialization oracle across the conformance subset, and that routing is genuinely cost-based (documented cases where each strategy is chosen).

**Open questions (carried from SPEC-23 §8):**
- **#4 recursive-fixpoint costing** — the blocking one: cost/cardinality/termination for fixpoint nodes on an additive scale built for non-recursive AGM/hash costing; how much is delegated opaque to the closure operator vs costed.
- No prior-art reference — this layer is HornDB-specific; design decisions must be justified from first principles, not borrowed.

**Acceptance criteria (derived from SPEC-23 §7 + §5.8):**
- **Result parity:** every rewrite/delegate plan returns results identical to the full-materialization oracle on the conformance subset (extends §7.1's no-regression discipline and §7.4's differential-oracle gate to the reasoning layer).
- **Cost-based routing:** materialize-vs-rewrite-vs-delegate is chosen by cost on the §5.5 additive scale, with at least one documented case per strategy where it is correctly selected.
- **Delegation correctness:** property-path/transitive closure delegated to the SPEC-05 GraphBLAS backend matches the materialized result and is chosen by selectivity (SPEC-07 F3).
- **Pass legibility carried forward (§7.2):** each new reasoning `PassId` is individually disable-able and validated after each pass.
