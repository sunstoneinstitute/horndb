---
status: draft
date: 2026-07-07
scope: "Phase 3: a layered read-only Stats seam over SPEC-02 + a Characteristic-Sets / degree-bound cardinality estimator (NDV+counts baseline), wired into EXPLAIN, demoting UniformEstimator to fallback"
---

# Statistics Seam + Cardinality Estimator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).
**SotA (state-of-the-art) reference:** [`docs/research/optimizer-sota.md`](../research/optimizer-sota.md) Part A — read it before task 1; it is the "why these algorithms" behind every choice here.

> ⚠️ **BLOCKED — not ready to execute.**
>
> This is a **design + task outline**, not an executable TDD (test-driven development) plan. Writing exact Rust against the not-yet-existent statistics surface would be fiction that rots. Do not expand into full TDD steps until the blockers below clear. The *algorithm* choices below are firm (SotA-grounded); the *code* is not.
>
> **Blocking dependencies:**
> - **SPEC-02 statistics surface is incomplete.** What exists today in `crates/storage/src/store.rs`: `triple_count()` (~L55), `stats() -> TierStats` (~L59), and `top_predicates(n) -> Vec<(Term,u64)>` (~L131). So **per-predicate counts are partially unblocked**; there is **no NDV (number of distinct values), no per-position selectivity, no characteristic-set index, and no degree summary** — those are the gap this phase depends on.
> - **SPEC-06 delta maintenance.** Characteristic Sets are **not naturally incremental** (§"Tier 1" below) — the hard part. Whoever populates the summary must keep it coherent under DBSP Z-set deltas, or accept periodic recompute. Undesigned here.
> - **SPEC-23 §8 open question #1 "SPEC-02 statistics ownership" BLOCKS this phase** — until it is resolved (does SPEC-02 grow these summaries as first-class, maintained incrementally under SPEC-06 deltas, or via periodic recompute?), the `Stats` impl has no source of truth.
>
> This plan may still be *drafted* against the layered `Stats` trait shape (§5.3) so the seam is ready the day SPEC-02 lands.

**Goal:** Replace the `_est`-ignoring coarse estimator with a real RDF-native cardinality
estimator over a **layered** read-only `Stats` seam on SPEC-02 storage, surfaced through the
existing EXPLAIN `~N rows` rendering. The estimator is **Characteristic-Sets-first** (CS —
the RDF SotA for star joins) with a **degree-sequence upper bound** (never under-estimates —
the property that actually protects plan quality) and a **sampling fallback** for non-star /
cold-summary shapes. NDV+counts is the *baseline tier*, not the destination.

## Architecture (SPEC-23 §5.3, §5.4; `docs/research/optimizer-sota.md` Part A)

**The `Stats` seam is layered and cost-tiered** so every technique can be added without
changing the seam. Each tier can be populated on its own; the estimator uses the best tier
available and degrades gracefully:

```rust
trait Stats {
    // Tier 0 — counts + NDV (the DuckDB "almost no statistics" baseline)
    fn total_triples(&self) -> u64;
    fn predicate_count(&self, p: TermId) -> u64;          // |{ t : t.p == p }|
    fn ndv(&self, p: TermId, pos: Position) -> u64;       // distinct S or O for predicate p

    // Tier 1 — Characteristic Sets (RDF-native SotA for star joins)
    //   lookup/enumerate characteristic sets C with count(C) and per-predicate occurrences.
    fn characteristic_sets(&self) -> &CharacteristicSetIndex;  // count(C), occurrences(C,p)

    // Tier 2 — per-predicate, per-role degree info for pessimistic (upper-bound) estimation
    fn max_degree(&self, p: TermId, role: Role) -> u64;        // bound-sketch input
    fn degree_sequence(&self, p: TermId, role: Role) -> Option<DegreeSummary>;  // SafeBound/LpBound (later)

    // Tier 3 — sampling hook (Wander-Join-style index walk); fallback, not default
    fn sample_join(&self, patterns: &[TriplePattern]) -> Option<(f64 /*est*/, f64 /*ci*/)>;
}
```

Estimators return **`(estimate, upper_bound)`** (not a bare number), so `JoinPlanning`
(PLAN-23-04) can prefer the bound when avoiding catastrophic under-estimates matters
(Leis et al.: under-estimates are what wreck plans) and the point estimate when tightness
matters.

**Estimator, by shape:**

- **Star join (shared subject, the RDF common case) → Characteristic Sets.**
  Neumann & Moerkotte, ICDE 2011. For query predicates `P` on `?s`:
  `card ≈ Σ_{C : P ⊆ C} count(C) · Π_{p∈P} m(C,p)` where `m(C,p) = occurrences(C,p)/count(C)`.
  A bound object scales its predicate by `≈ 1/ndv(p, Object)` instead of `m(C,p)`. This
  captures **predicate correlation for free** — the single biggest accuracy lever on RDF and
  the concrete replacement for `UniformEstimator`'s `1/16`-per-bound-position independence
  model. ⚠ verify the formula against the ICDE'11 PDF before coding (egress blocked full text).
- **General / multi-join → DuckDB denominator model as the Tier-0 baseline**, upgraded where
  a characteristic-set superset match exists. `∏ base / denominator`, denominator per shared
  variable `≈ max(ndv)`, **with transitive-equality-class tracking** (a variable shared across
  ≥3 patterns — the RDF star/chain norm — divided once, not per pair) and the
  **PK/FK (primary-key/foreign-key) cap**
  (an `owl:sameAs` / functional-property / key join never exceeds the smaller input; essential
  for RDF — sameAs closures otherwise explode). This is the fallback when CS does not cover the
  shape; SumRDF-style whole-graph summarisation (Stefanoni/Motik/Kostylev, WWW 2018) is the
  design-for upgrade for non-star shapes, slotting behind the same CS interface.
- **Upper bound (always available) → degree-sequence / AGM bound.** `max_degree` feeds a
  bound-sketch upper bound (Cai/Balazinska/Suciu, SIGMOD 2019) — cheap, never under-estimates,
  and speaks leapfrog triejoin's native AGM language. `degree_sequence` feeds the
  SafeBound/LpBound tightening (design-for, PLAN-23-05). The AGM LP itself is computed in
  PLAN-23-04's cost stage, not here.
- **Cold / non-star / low-confidence → `sample_join`** (Wander Join, Li et al., SIGMOD 2016).
  G-CARE (Park et al., SIGMOD 2020) found sampling beats CS/SumRDF on real graphs — so it is
  the accuracy *backstop*, exposed as a hook, implemented as a targeted fallback (per-query
  cost + variance rule it out as the default).
- **Learned estimators: out of scope** (Wang et al., PVLDB 2021: not production-ready for a
  continuously-materializing store). The trait admits one later as "just another impl" at zero
  present cost — build nothing.

Estimates are memoized (cached) by variable/pattern bitset (DuckDB's `relation_set_2_cardinality`).
This phase also **unifies the two disconnected cardinality surfaces** that exist today:
`Executor::cardinality_estimate` (in `horndb-sparql`, EXPLAIN-only) and
`wcoj::Cardinality`/`UniformEstimator` (plumbed but ignored in `Planner::choose`'s `_est`).

**Tech stack / crates touched:** `horndb-wcoj` (`crates/wcoj/src/cardinality.rs` — the layered
`Stats` trait, the CS/denominator/bound estimator, demote `UniformEstimator` to the zero-stats
fallback); `horndb-storage` (the `Stats` impl + the characteristic-set index and degree
summaries reading SPEC-02 — gated on SPEC-02 landing them); `horndb-sparql`
(`crates/sparql/src/plan/explain.rs::estimate` — wire real estimates into EXPLAIN, retire the
disconnected `Executor::cardinality_estimate` path).

**Depends on:**
- [PLAN-23-01] / [PLAN-23-02] — the framework scaffolding (logical IR, pass registry, binding/type lattice) and heuristic rewrite passes.
- **SPEC-02** — must expose per-predicate counts, per-position NDV, the characteristic-set index, and per-predicate-per-role degree summaries (counts partially present; the rest absent).
- **SPEC-06** — incremental maintenance (or periodic recompute) of all of the above under DBSP deltas; the CS-is-not-incremental problem is the sharp edge.
- SPEC-23 §5.3 (Stats seam), §5.4 (estimator), §8 open question #1; `docs/research/optimizer-sota.md` Part A.

**Prerequisites to unblock:**
- [ ] SPEC-02 exposes per-position NDV (distinct S / distinct O per predicate), maintainable as HyperLogLog (HLL) distinct-count sketches over dictionary IDs.
- [ ] SPEC-02 exposes a **characteristic-set index** — distinct sets `C` with `count(C)` and per-predicate `occurrences(C,p)`, top-K-capped with a residual bucket for the rare-set tail.
- [ ] SPEC-02 exposes per-predicate, per-role `max_degree` (and, for the design-for tier, a compressed degree sequence).
- [ ] SPEC-23 §8 #1 resolved: ownership + maintenance model (incremental-under-deltas vs periodic recompute) for **all** the above summaries — decided and recorded in SPEC-02/SPEC-06. The CS incremental-maintenance decision is the hard one.
- [ ] A baseline EXPLAIN-vs-measured accuracy run exists to set the ≥X% threshold in §7.3.
- [ ] The `sparql ↔ wcoj` API boundary question (§8 #3) has at least a provisional answer for how estimates cross the crate seam (does `wcoj` see `Stats` directly, or a digested per-pattern estimate?).

## Task outline

1. **Define the layered `Stats` trait + a stub impl.** Add the trait (all four tiers) in
   `crates/wcoj/src/cardinality.rs` (or a new `stats.rs`), plus a `ZeroStats` stub returning
   conservative constants (and `None` for the optional tiers) so downstream code compiles
   before SPEC-02 lands. Keep `Position`/`Role` aligned with the storage triple layout.
2. **Tier 0 — storage-backed counts + NDV.** Implement Tier 0 over SPEC-02 in
   `horndb-storage`: `predicate_count` from existing per-predicate counts, `ndv` from the new
   NDV surface. Gate behind a "stats populated" check; default to the fallback otherwise (the
   ClickHouse lesson: an unmaintained stats feature is dead weight).
3. **Tier 0 estimator — base cardinality.** Per-pattern base card from `predicate_count`/`ndv`,
   falling back to the `sparopt` 8-entry static shape table when the predicate is unbound.
   Replace `UniformEstimator`'s `1/16`-per-bound-position model as the default; keep it as the
   named zero-stats fallback.
4. **Tier 0 estimator — join output (denominator model).** `∏ base / denominator`; denominator
   per shared variable `≈ max(ndv)`. Add transitive-equality-class tracking and the PK/FK cap.
   Memoize by variable/pattern bitset. This is the general-shape baseline the CS estimator
   improves on.
5. **Tier 1 — Characteristic Sets.** Build the CS index in `horndb-storage` (grouped scan by
   subject; top-K + residual bucket) and the star-join estimator in the `Cardinality` impl.
   Detect star shapes in the BGP (shared-subject patterns) and route them to CS; everything
   else keeps the task-4 denominator estimate. ⚠ verify the estimation formula against the
   ICDE'11 PDF first. Unit-test on a synthetic graph with known implicit types.
6. **Tier 2 — degree-based upper bound.** `max_degree` per predicate-role → a bound-sketch
   upper bound; make the estimator return `(estimate, upper_bound)`. (Full degree-sequence /
   LpBound tightening is PLAN-23-05.)
7. **Tier 3 — `sample_join` fallback (hook + light impl).** Wander-Join-style index walk over
   the sorted permutation indexes, returning `(estimate, confidence)`; invoked only when the
   summary tiers are absent or flag low confidence for the shape. Keep it off the default path.
8. **Wire estimates into EXPLAIN.** Route the real estimator into
   `crates/sparql/src/plan/explain.rs::estimate`, retiring the separate EXPLAIN-only
   `Executor::cardinality_estimate` so there is one cardinality surface. Golden-EXPLAIN
   snapshots update in this task.
9. **Accuracy harness + threshold.** Add a harness check comparing EXPLAIN estimates to
   measured row counts on the conformance subset (and, if available, a G-CARE-style subset);
   record the baseline, lock the §7.3 threshold, and prove **strictly better than
   `UniformEstimator`** — reporting CS-vs-denominator separately so the CS win is visible.

## Open questions (carried from SPEC-23 §8, plus SotA-specific)

- **#1 SPEC-02 statistics ownership** — the blocking one, now covering NDV **and** the
  characteristic-set index **and** degree summaries: first-class in SPEC-02, incremental under
  SPEC-06 deltas vs periodic recompute. The **CS-is-not-incremental** problem (one triple can
  move a subject between characteristic sets) is the hardest sub-question.
- **#3 sparql/wcoj API boundary** — does `wcoj` consult `Stats` directly or receive a digested
  per-pattern estimate? Pin before task 4.
- **CS memory cap tuning** — the top-K + residual split trades accuracy for bounded memory on
  heterogeneous graphs; K needs a data-driven default from a real dataset.

## Acceptance criteria (SPEC-23 §7.3)

- On the conformance subset (`harness/selected.toml`), EXPLAIN cardinality estimates are within
  an order of magnitude of measured row counts on ≥ X% of nodes (threshold TBD from a baseline
  run) — and **strictly better than `UniformEstimator`**, with the Characteristic-Sets
  estimator beating the Tier-0 denominator model on star shapes specifically.
- The estimator **never under-estimates below its reported `upper_bound`** on the tested shapes
  (the degree-bound guarantee).
- `UniformEstimator` remains available as the zero-stats fallback and is only demoted (not
  deleted) once the stats-backed estimator is proven at least as good on the harness.
