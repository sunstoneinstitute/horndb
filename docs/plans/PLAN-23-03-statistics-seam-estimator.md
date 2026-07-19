---
status: in-progress
date: 2026-07-07
scope: "Phase 3: a layered read-only Stats seam + a Characteristic-Sets / degree-bound cardinality estimator (NDV+counts baseline), computed recompute-from-snapshot, wired into EXPLAIN, demoting UniformEstimator to fallback"
---

# Statistics Seam + Cardinality Estimator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).
**SotA (state-of-the-art) reference:** [`docs/research/optimizer-sota.md`](../research/optimizer-sota.md) Part A — read it before task 1; it is the "why these algorithms" behind every choice here.

> ✅ **UNBLOCKED via recompute-from-snapshot (SPEC-23 §8 #1 provisional resolution).**
>
> **Resolution of the ownership question (§8 #1), provisional, for review with SPEC-02/SPEC-06:**
> the `Stats` impl reads a **read-only, computed-from-the-pinned-snapshot** summary. Statistics
> (per-predicate counts, per-position NDV, the Characteristic-Sets index, per-role max-degree)
> are computed by **scanning the snapshot the query executor already materializes**, cached per
> snapshot, and **recomputed when the snapshot changes** — no incremental maintenance under
> SPEC-06 deltas. This sidesteps the CS-is-not-incremental problem (the hard sub-question):
> a recompute is always coherent because it reads one immutable snapshot. Incremental
> maintenance stays a future SPEC-06 coordination item; recompute-from-snapshot is the
> conservative default that unblocks the estimator now.
>
> **Where the stats are computed — deviation from "in `horndb-storage`".** The crate
> dependency direction is `storage → wcoj`, so `horndb-storage` **cannot** implement a `Stats`
> trait defined in `horndb-wcoj`. Therefore the seam, the stat data types, and the
> recompute-from-snapshot impl all live in **`horndb-wcoj`**, computed from the
> `TripleSource` snapshot (`VecTripleSource`, the materialized view the executor already builds
> via `wcoj_snapshot()`). This keeps everything in wcoj's `TermId` space and makes the stats
> **consistent by construction** with the measured `count_bgp` numbers the accuracy gate
> compares against. `horndb-storage` is unchanged.
>
> The *algorithm* choices below are firm (SotA-grounded). The executable task plan is at the
> end of this document (**"Executable task plan"**).

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

**Locked baseline (Task 8 gate).** The accuracy gate lives in-crate as
`crates/wcoj/src/estimator.rs` (`mod accuracy_gate::accuracy_gate_spec23_acceptance_3`).
It runs a representative shape suite over a synthetic graph with predicate correlation
(implicit types A/B) and grades the stats estimator against the ground-truth oracle
(`brute_force_count`). Measured within-an-order-of-magnitude fraction = **1.0** (all five
shape estimates exact on this regular correlated graph); **0.8 is the locked threshold**
carried forward from this baseline. The gate also asserts: stats mean |log-ratio| error
(0.0000) strictly below uniform's (1.2876); CS star error (0.0000) ≤ denominator error
(0.8283), strictly lower on the correlated star; and `upper_bound ≥ measured` on every shape.

---

## Executable task plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`.
> Execute task-by-task; each task is TDD (red → green → commit). Types/signatures below are
> firm; fill bodies via TDD. Everything lives in **`horndb-wcoj`** except EXPLAIN wiring
> (`horndb-sparql`) and the accuracy check (`horndb-harness`). `horndb-storage` is untouched.

**Goal:** A layered read-only `Stats` seam + a Characteristic-Sets/degree-bound cardinality
estimator, computed recompute-from-snapshot from the wcoj `TripleSource`, wired into `EXPLAIN`,
with `UniformEstimator` demoted to the zero-stats fallback and an accuracy gate proving the new
estimator strictly better than uniform (CS beats the Tier-0 denominator model on star shapes;
`upper_bound` never below measured).

**Key types (all in `crates/wcoj/src/stats.rs`, `TermId = u64`):**

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position { Subject, Object }   // predicate is always bound in per-predicate stats
pub type Role = Position;               // degree role; same axis

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Estimate { pub estimate: u64, pub upper_bound: u64 }

/// One characteristic set: the exact predicate-set of some group of subjects.
pub struct CharacteristicSet {
    pub predicates: Vec<TermId>,          // sorted, distinct — the set key
    pub count: u64,                       // #subjects with exactly this predicate-set
    pub occurrences: Vec<(TermId, u64)>,  // sorted by pred: total objects for that pred across the count subjects
}

/// Top-K frequent sets + a residual bucket folding the rare-set tail.
pub struct CharacteristicSetIndex {
    pub sets: Vec<CharacteristicSet>,     // top-K by count, descending
    pub residual_subjects: u64,           // #subjects in the folded tail
    pub residual_pred_occ: Vec<(TermId, u64)>, // pred -> object occurrences within the tail
    // predicate -> indices into `sets` that contain it (superset-lookup accelerator)
    pub by_predicate: std::collections::HashMap<TermId, Vec<usize>>,
}

pub struct DegreeSummary; // Tier-2 design-for stub (SafeBound/LpBound is PLAN-23-05)

pub trait Stats: Send + Sync {
    fn total_triples(&self) -> u64;
    fn predicate_count(&self, p: TermId) -> u64;
    fn ndv(&self, p: TermId, pos: Position) -> u64;
    fn characteristic_sets(&self) -> &CharacteristicSetIndex;
    fn max_degree(&self, p: TermId, role: Role) -> u64;
    fn degree_sequence(&self, _p: TermId, _role: Role) -> Option<DegreeSummary> { None }
    fn sample_join(&self, _patterns: &[TriplePattern]) -> Option<(f64, f64)> { None }
}
```

### Task 1 — `Stats` trait, data types, `ZeroStats` fallback

**Files:** Create `crates/wcoj/src/stats.rs`; modify `crates/wcoj/src/lib.rs` (add `pub mod stats;`).

- [ ] Write failing unit test `zero_stats_is_conservative`: a `ZeroStats::new(total)` returns
  `total_triples()==total`, `predicate_count(p)` a conservative non-zero (documented: `total`),
  `ndv(p,pos)==1` (most-conservative denominator → never divides output down spuriously),
  `characteristic_sets().sets` empty, `max_degree(..)==total` (loosest bound). Run: `cargo test -p horndb-wcoj zero_stats_is_conservative` → FAIL (no `stats` module).
- [ ] Implement the types above + `ZeroStats { total, empty_index: CharacteristicSetIndex }`.
  `CharacteristicSetIndex::empty()` helper. `Estimate`, `Position`, `Role`.
- [ ] Green + `cargo fmt --all` + commit: `feat(wcoj): layered Stats seam + ZeroStats fallback`.

### Task 2 — expose the snapshot rows + `SnapshotStats` Tier 0 (counts + NDV)

**Files:** modify `crates/wcoj/src/source/vec_source.rs` (add `pub fn sorted_rows(&self, ord: Ordering) -> Option<&[(TermId, TermId, TermId)]>`); modify `crates/wcoj/src/stats.rs`.

- [ ] Test `snapshot_stats_tier0`: build a `VecTripleSource` from a small known triple set
  (e.g. preds p1 on 3 subjects with 2 distinct objects each; p2 on 1 subject). Assert
  `predicate_count(p1)==6`, `ndv(p1, Subject)==3`, `ndv(p1, Object)==2`, `total_triples()`.
  Run → FAIL.
- [ ] Add `VecTripleSource::sorted_rows`. Implement `SnapshotStats::from_source(&dyn TripleSource)`
  (or `from_vec_source(&VecTripleSource)`): scan `Pso` slice for per-predicate counts + distinct
  subjects (NDV Subject), `Pos` slice for distinct objects (NDV Object). Store in `HashMap<TermId,…>`.
  Exact distinct counts over the sorted slice (adjacent-dedupe — no HLL needed at snapshot scale;
  note HLL as the future incremental path). Cache computed index in the struct.
- [ ] Green + fmt + commit: `feat(wcoj): SnapshotStats Tier 0 counts+NDV from the pinned snapshot`.

### Task 3 — `SnapshotStats` Tier 1 (Characteristic-Sets index) + Tier 2 (max_degree)

**Files:** modify `crates/wcoj/src/stats.rs`.

- [ ] Test `characteristic_sets_grouping`: from a graph where subjects s1,s2 have predicate-set
  {p1,p2} and s3 has {p1}, assert the index has a set `{p1,p2}` with `count==2` and one `{p1}`
  with `count==1`; `occurrences` sums objects correctly; `by_predicate[p1]` lists both set indices.
  Test `max_degree_basic`: `max_degree(p1, Subject)` == max #objects any single subject has on p1.
  Run → FAIL.
- [ ] Implement CS build: scan `Spo` slice grouped by subject; for each subject collect its
  distinct predicate-set + per-pred object counts; aggregate identical predicate-sets. Keep top-K
  (const `CS_TOP_K: usize = 1024`) by count; fold the rest into `residual_*`. Build `by_predicate`.
  Implement `max_degree` from a per-(pred,role) max computed in the same scans.
- [ ] Green + fmt + commit: `feat(wcoj): SnapshotStats Tier 1 characteristic sets + Tier 2 max_degree`.

### Task 4 — `StatsEstimator`: per-pattern base + denominator join model (equality classes + PK/FK cap)

**Files:** Create `crates/wcoj/src/estimator.rs`; modify `crates/wcoj/src/lib.rs`.

`StatsEstimator<'a, S: Stats>` holds `&'a S` + a memo `HashMap<u64 /*pattern bitset*/, Estimate>`.

- Per-pattern base card (`estimate_pattern`): predicate bound → `predicate_count(p)` scaled by
  bound S/O via `1/ndv(p,pos)` per bound endpoint (so `?s p o` ≈ `predicate_count/ndv(p,Object)`,
  `s p o` ≈ `max(1, predicate_count/(ndv_s*ndv_o))`); predicate unbound → the `sparopt` 8-entry
  static shape table by which of S/P/O are bound (document the constants inline).
- Join output (denominator model): `∏ base / denom`. For each variable shared across patterns,
  `denom_var ≈ max(ndv over the patterns binding it)`; a variable shared across ≥3 patterns is
  divided **once** (transitive equality class), not per pair. PK/FK cap: the joined estimate for
  patterns sharing a var never exceeds the smaller participating base (protects `owl:sameAs` /
  functional-property joins). Memoize by pattern-index bitset.

- [ ] Test `estimate_pattern_matches_counts` (single bound-predicate pattern ≈ predicate_count),
  `denominator_join_shrinks` (2-pattern star estimate ≤ product, ≥ larger base), `pkfk_cap`
  (a sameAs-shaped join capped at the smaller input), `memoized` (same bitset → cached). Run → FAIL.
- [ ] Implement. Green + fmt + commit: `feat(wcoj): StatsEstimator base + denominator join model`.

### Task 5 — CS star-join estimator + degree upper bound → `(estimate, upper_bound)`

**Files:** modify `crates/wcoj/src/estimator.rs`.

- Star detection: a BGP sub-group of patterns sharing one subject variable with bound predicates.
  Route to CS: for query predicate set `P` on `?s`,
  `est ≈ Σ_{C : P ⊆ C.predicates} C.count · Π_{p∈P} (occ(C,p)/C.count)` (+ residual-bucket term).
  ⚠ **Verify this formula against Neumann & Moerkotte ICDE'11 before coding** (§5.4 note; the
  survey's reconstruction carries a ⚠). Non-star groups keep the task-4 denominator estimate.
- Upper bound (always set): degree-based — `Π max_degree(p, role)` over the star's bound
  predicates, capped at `total_triples()`; for the denominator path, the product of per-pattern
  bases capped likewise. `estimate` ≤ `upper_bound` must hold; `upper_bound` ≥ any achievable size.

- [ ] Test `cs_beats_denominator_on_star`: on a synthetic graph with correlated predicates
  (implicit "type" — subjects that have p1 always have p2), the CS star estimate is closer to the
  true `count_bgp` than the denominator estimate. Test `upper_bound_never_below_measured`: over a
  set of shapes, `upper_bound >= measured` always. Run → FAIL.
- [ ] Implement (verify the CS formula first; record the check in the commit body). Green + fmt +
  commit: `feat(wcoj): characteristic-sets star estimator + degree upper bound`.

### Task 6 — Tier-3 `sample_join` hook (stub, off the default path)

**Files:** modify `crates/wcoj/src/stats.rs`.

- [ ] Keep `sample_join` as the trait default returning `None` for `ZeroStats`; for `SnapshotStats`
  provide a **documented light Wander-Join-style** walk **behind a `cfg`/opt-in flag returning
  `None` by default** (the plan: hook now, not on the default path). Add a `#[ignore]`-free unit
  test asserting it returns `None` unless explicitly enabled, so the hook is covered but inert.
- [ ] Green + fmt + commit: `feat(wcoj): sample_join Tier-3 hook (inert by default)`.

### Task 7 — wire the estimator into EXPLAIN (unify the cardinality surface)

**Files:** modify `crates/sparql/src/exec/horn.rs` (`cardinality_estimate`), `crates/sparql/src/plan/explain.rs`, and golden EXPLAIN snapshots under `crates/sparql/` (update in this task).

- [ ] In `horn.rs::cardinality_estimate`, build a `SnapshotStats` from `self.wcoj_snapshot()`,
  translate the sparql `TriplePattern`s to wcoj patterns (reuse the existing translation in
  `count_bgp`/`scan_bgp_ids`), run `StatsEstimator::estimate_bgp`, and return the **point
  estimate** (`Estimate.estimate`) as `Some(usize)`. Gate: if the snapshot is empty or stats are
  degenerate, fall back to the old coarse bound (keep `UniformEstimator` semantics reachable).
- [ ] Keep the `~N rows` rendering; optionally append `(≤U)` upper bound in the EXPLAIN text if the
  golden format allows — otherwise leave rendering untouched to minimise snapshot churn.
- [ ] Update golden snapshots. Run `cargo test -p horndb-sparql` and (if server touched)
  `cargo test -p horndb-sparql --features server`. Green + fmt + commit:
  `feat(sparql): EXPLAIN uses the stats-backed cardinality estimator`.

### Task 8 — accuracy gate on the conformance subset (harness) + baseline threshold

**Files:** add a test under `crates/harness/tests/` (e.g. `cardinality_accuracy.rs`) or a harness
subcommand; consult `crates/harness/CLAUDE.md` for suite wiring.

- [ ] For each BGP-bearing query in the conformance subset, compute measured rows via the existing
  count path and both estimates (uniform vs stats). Assert, as the gate: (a) the stats estimator's
  aggregate |log-ratio| error is **strictly less** than uniform's; (b) on star-shaped queries the
  CS estimate's error < the denominator estimate's error; (c) `upper_bound >= measured` on every
  tested node. Record the measured threshold X% (within-an-order-of-magnitude fraction) in the test
  and in `docs/benchmarks.md`/this plan's acceptance section. Run the harness locally (production
  crates) to confirm green.
- [ ] Green + fmt + commit: `test(harness): EXPLAIN cardinality accuracy gate (stats > uniform)`.

### Task 9 — docs sync + plan close-out

**Files:** `docs/architecture.md` (flip the SPEC-23 phase-3 / statistics-seam row to
**implemented**), `docs/specs/SPEC-23-unified-ir.md` (§8 #1: note the provisional
recompute-from-snapshot resolution + pointer here), this plan (`status: executed`), and
`docs/index.md` if a new doc was added. **Do not touch `TASKS.md`** (lock-serialized on `main`).

- [ ] Update docs; `cargo fmt --all`; commit: `docs: SPEC-23 phase-3 stats seam implemented; §8 #1 resolved (recompute-from-snapshot)`.

## Acceptance mapping (SPEC-23 §7.3 / acceptance #3)

- *within an order of magnitude, ≥ X% of nodes, strictly better than `UniformEstimator`* → Task 8 (a).
- *CS beats the Tier-0 denominator model on star shapes* → Task 5 test + Task 8 (b).
- *`upper_bound` never below measured* → Task 5 test + Task 8 (c).
- *`UniformEstimator` demoted, not deleted; fallback default until proven* → Task 1 (`ZeroStats`),
  Task 7 gate/fallback, `UniformEstimator` retained in `cardinality.rs`.
