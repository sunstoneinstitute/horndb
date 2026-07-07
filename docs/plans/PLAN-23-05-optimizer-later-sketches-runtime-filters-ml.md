---
status: draft
date: 2026-07-07
scope: "Phase 5: later optimizer work — statistics sketches behind Stats, runtime filters (sideways information passing), and the ML PlanAdvisor validation loop"
---

# Later Optimizer Work (Sketches / Runtime Filters / ML) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **skeleton** (task outline only), not an executable TDD plan. It bundles three loosely-coupled sub-tracks; each becomes its own executable plan once its prerequisites land.
>
> **Blocking dependencies:**
> - **[PLAN-23-03] (Stats seam + estimator) and [PLAN-23-04] (cost-based JoinPlanning) must land first.** Sketches extend the `Stats` seam; runtime filters and the ML advisor both hang off the cost-based `JoinPlanning` stage.
> - **Sub-track (a) sketches** additionally inherits the SPEC-02 statistics-ownership question (SPEC-23 §8 #1) — sketch maintenance under SPEC-06 deltas is undesigned.
> - **Sub-track (c) ML** requires wiring a new `horndb-sparql → horndb-ml` crate dependency that does not exist today.

**Goal:** Extend the shipped optimizer with three later-phase capabilities behind the existing seams — quantile/count-min statistics sketches, runtime filters (sideways information passing), and an ML `PlanAdvisor` validation loop — each individually disable-able and each preserving plan neutrality when off.

**Architecture (SPEC-23 §5.6, §5.7):** Three loosely-coupled sub-tracks. **(a) Statistics sketches** — quantile (TDigest) and count-min sketches added behind the same `Stats` trait (§5.3) for range and equality-constant selectivity, the DuckDB→ClickHouse upgrade path from NDV+counts. **(b) Runtime filters / sideways-information-passing (§5.6)** — build a set/bloom from one pattern's bindings and push it as a skip filter into another pattern's scan; a new `PassId`; maps onto RDF star joins and `FILTER EXISTS`, and is a natural fit for WCOJ intermediate bindings; borrows ClickHouse's `tryAddJoinRuntimeFilter` and any-join→semi/anti rewrites. **(c) ML `PlanAdvisor` validation loop (§5.7)** — `JoinPlanning` constructs a `SubplanShape` and calls `registry.plan_advisor().advise(...)`, treating the result as a *hint* validated against the `Stats`-backed estimate and discarded past tolerance or the 1 ms p99 budget. The current ML seam already exists: `crates/ml/src/planner.rs` has the `PlanAdvisor` trait + `DisabledPlanAdvisor`, and `crates/ml/src/types.rs` has `SubplanShape { n_patterns, n_vars, bound_vars }` / `PlanAdvice`. `horndb-sparql` does **not** depend on `horndb-ml` today — wiring that dependency is part of this phase.

**Tech stack / crates touched:** `horndb-storage` + `horndb-wcoj` (sketch impls behind `Stats`); `horndb-wcoj` / `horndb-sparql` (new runtime-filter `PassId`, set/bloom build + scan-side skip filter); `horndb-sparql` + `horndb-ml` (new `sparql → ml` dep, `PlanAdvisor` call site in `JoinPlanning`, validation-against-Stats + p99 skip budget).

**Depends on:**
- [PLAN-23-03] — the `Stats` seam (sketches extend it).
- [PLAN-23-04] — cost-based `JoinPlanning` (runtime filters and the ML advisor plug into it).
- **SPEC-08 F2** — the `PlanAdvisor` contract (symbolic is source of truth; advice is a validated hint).
- SPEC-23 §5.6, §5.7, §7.2, §7.6.

**Prerequisites to unblock:**
- [ ] [PLAN-23-03] and [PLAN-23-04] executed.
- [ ] (a) SPEC-02/SPEC-06 sketch maintenance model decided (same §8 #1 ownership question, extended to TDigest/count-min).
- [ ] (b) A representative RDF star-join / `FILTER EXISTS` workload identified to prove a runtime-filter win.
- [ ] (c) The `sparql → ml` dependency direction reviewed against the crate dependency order (`ml` sits on top today; confirm no cycle) and the 1 ms p99 skip-budget measurement harness exists.

**Task outline:**
1. **TDigest / count-min sketches behind `Stats`.** Add quantile (range-predicate selectivity) and count-min (equality-constant selectivity) methods to the `Stats` trait and impls; keep them optional so the NDV+counts estimator degrades gracefully when sketches are absent.
2. **Runtime-filter pass.** New `PassId` (§5.6): build a set/bloom from one pattern's bindings, push it as a scan-side skip filter into another pattern; wire the any-join→semi/anti and join→`IN` rewrites (ClickHouse `tryAddJoinRuntimeFilter`). Target WCOJ intermediate bindings first. Must be individually disable-able (§7.2).
3. **Wire the `horndb-ml` dependency into `horndb-sparql`.** Add the crate dep; expose the registry's `plan_advisor()` (defaulting to `DisabledPlanAdvisor`).
4. **ML `PlanAdvisor` validation loop.** In `JoinPlanning`, build a `SubplanShape`, call `advise(...)`, validate the returned `PlanAdvice` against the `Stats`-backed estimate, discard past tolerance, and skip entirely if it blows the 1 ms p99 budget. The symbolic plan remains source of truth.
5. **Neutrality + disable-ability harness.** Prove that with `ml.enabled = false` plans are bit-identical to a no-ML build, that each new `PassId` is individually disable-able, and that the advisor path respects the 1 ms p99 skip budget.

**Open questions (carried from SPEC-23 §8):**
- **#1 SPEC-02 statistics ownership** — extended to sketch maintenance (TDigest/count-min) under SPEC-06 deltas.
- (ML) how tolerance for "implausible advice" is set relative to the `Stats` estimate's own confidence — a calibration question surfacing once real estimates exist.

**Acceptance criteria (SPEC-23 §7.6, §7.2):**
- **ML neutrality:** with `ml.enabled = false`, plans are **bit-identical** to a no-ML build (SPEC-08 F2), and the advisor path respects the 1 ms p99 skip budget.
- **Pass legibility (§7.2):** every new `PassId` (runtime filters) is individually disable-able via config/pragma; the driver still asserts declared ordering constraints; debug builds validate the IR after each new pass; a regression can be bisected to a single `PassId`.
