---
status: draft
date: 2026-07-07
scope: "Phase 3: a read-only Stats seam over SPEC-02 + an NDV/counts cardinality estimator, wired into EXPLAIN, demoting UniformEstimator to fallback"
---

# Statistics Seam + NDV/Counts Estimator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **skeleton** (task outline only), not an executable TDD plan. Writing exact Rust against the not-yet-existent statistics surface would be fiction that rots. Do not expand into full steps until the blockers below clear.
>
> **Blocking dependencies:**
> - **SPEC-02 statistics surface (the NDV half) does not exist.** What exists today in `crates/storage/src/store.rs`: `triple_count()` (~L55), `stats() -> TierStats` (~L59), and `top_predicates(n) -> Vec<(Term,u64)>` (~L131). So **per-predicate counts are partially unblocked** already; there is **no NDV / histogram / per-position selectivity** surface at all — that is the gap this phase depends on.
> - **SPEC-06 delta maintenance.** Whoever populates counts/NDV must keep them coherent under DBSP Z-set deltas (insertion + retraction). This is not designed here.
> - **SPEC-23 §8 open question #1 "SPEC-02 statistics ownership" BLOCKS this phase** — until it is resolved (does SPEC-02 grow counts/NDV as first-class, maintained incrementally under SPEC-06 deltas, or via periodic recompute?), the `Stats` impl backing this estimator has no source of truth.
>
> This plan may still be *drafted* against the `Stats` trait shape (§5.3) so the seam is ready the day SPEC-02 lands.

**Goal:** Replace the `_est`-ignoring coarse estimator with a real NDV+counts cardinality estimator over a read-only `Stats` seam on SPEC-02 storage, surfaced through the existing EXPLAIN `~N rows` rendering.

**Architecture (SPEC-23 §5.3, §5.4):** Introduce a read-only `trait Stats { fn total_triples(&self)->u64; fn predicate_count(&self, p: TermId)->u64; fn ndv(&self, p: TermId, pos: Position)->u64; }` — a seam over SPEC-02 (stubbed where unpopulated). A real `Cardinality` estimator sits on top: per-pattern base cardinality from `predicate_count`/`ndv` (falling back to the `sparopt` static shape table when the predicate is unbound), join output via the DuckDB denominator model `∏ base / denominator` (denominator per shared variable ≈ `max(ndv)`), extended with **transitive-equality-class tracking** (a variable shared across ≥3 patterns is divided once, not per pair) and the **PK/FK-style cap** (an `owl:sameAs` / functional-property / key join never exceeds the smaller input). Estimates are memoized by variable/pattern bitset. This phase also **unifies the two disconnected cardinality surfaces** that exist today: `Executor::cardinality_estimate` (in `horndb-sparql`, EXPLAIN-only) and `wcoj::Cardinality`/`UniformEstimator` (plumbed but ignored in `Planner::choose`'s `_est`).

**Tech stack / crates touched:** `horndb-wcoj` (`crates/wcoj/src/cardinality.rs` — new `Stats` trait, real estimator, demote `UniformEstimator` to the zero-stats fallback); `horndb-storage` (the `Stats` impl reading SPEC-02 counts/NDV — gated on SPEC-02 landing them); `horndb-sparql` (`crates/sparql/src/plan/explain.rs::estimate` — wire real estimates into EXPLAIN, retire the disconnected `Executor::cardinality_estimate` path).

**Depends on:**
- [PLAN-23-01] / [PLAN-23-02] — the framework scaffolding (logical IR, pass registry, binding/type lattice) and heuristic rewrite passes.
- **SPEC-02** — must expose per-predicate counts and per-position NDV (counts partially present; NDV absent).
- **SPEC-06** — incremental maintenance of counts/NDV under DBSP deltas.
- SPEC-23 §5.3 (Stats seam), §5.4 (estimator), §8 open question #1.

**Prerequisites to unblock:**
- [ ] SPEC-02 exposes per-position NDV (distinct S / distinct O per predicate), maintainable as HLL over dictionary IDs.
- [ ] SPEC-23 §8 #1 resolved: ownership + maintenance model for counts/NDV (incremental-under-deltas vs periodic recompute) decided and recorded in SPEC-02/SPEC-06.
- [ ] A baseline EXPLAIN-vs-measured accuracy run exists to set the ≥X% threshold in §7.3.
- [ ] The `sparql ↔ wcoj` API boundary question (§8 #3) has at least a provisional answer for how estimates cross the crate seam (does `wcoj` see `Stats` directly, or a digested per-pattern estimate?).

**Task outline:**
1. **Define the `Stats` trait and a stub impl.** Add `trait Stats` (§5.3) in `crates/wcoj/src/cardinality.rs` (or a new `stats.rs`), plus a `ZeroStats`/stub impl returning conservative constants so downstream code compiles before SPEC-02 lands. Keep `Position` aligned with the storage triple layout.
2. **Storage-backed `Stats` impl.** Implement `Stats` over SPEC-02 in `horndb-storage`, reading `predicate_count` from existing per-predicate counts and `ndv` from the new NDV surface. Gate behind a "stats populated" check; default to fallback otherwise (the ClickHouse lesson: an unmaintained stats feature is dead weight).
3. **Real base-cardinality estimator.** Per-pattern base card from `predicate_count`/`ndv`, falling back to the `sparopt` 8-entry static shape table when the predicate is unbound. Replace `UniformEstimator`'s `1/16`-per-bound-position model as the default, keeping it as the named zero-stats fallback.
4. **Join-output estimator with the DuckDB denominator model.** `∏ base / denominator`; denominator per shared variable ≈ `max(ndv)`. Add transitive-equality-class tracking and the PK/FK cap (owl:sameAs / functional property / key join ≤ smaller input). Memoize by variable/pattern bitset (DuckDB's `relation_set_2_cardinality`).
5. **Wire estimates into EXPLAIN.** Route the real estimator into `crates/sparql/src/plan/explain.rs::estimate`, retiring the separate EXPLAIN-only `Executor::cardinality_estimate` so there is one cardinality surface. Golden-EXPLAIN snapshots update in this task.
6. **Accuracy harness + threshold.** Add a harness check comparing EXPLAIN estimates to measured row counts on the conformance subset; record the baseline and lock the §7.3 threshold. Prove strictly-better-than-`UniformEstimator`.

**Open questions (carried from SPEC-23 §8):**
- **#1 SPEC-02 statistics ownership** — the blocking one: first-class counts/NDV in SPEC-02, incremental under SPEC-06 deltas vs periodic recompute.
- **#3 sparql/wcoj API boundary** — does `wcoj` consult `Stats` directly or receive a digested per-pattern estimate? Pin before task 4.

**Acceptance criteria (SPEC-23 §7.3):**
- On the conformance subset (`harness/selected.toml`), EXPLAIN cardinality estimates are within an order of magnitude of measured row counts on ≥ X% of nodes (threshold TBD from a baseline run) — and **strictly better than `UniformEstimator`**.
- `UniformEstimator` remains available as the zero-stats fallback and is only demoted (not deleted) once the stats-backed estimator is proven at least as good on the harness.
