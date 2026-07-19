---
status: executed
date: 2026-07-19
scope: "SPEC-24 S1 — delta-incremental rule retraction: incremental distinct via a per-row derivation-weight trace, per-plan integrated operator traces, one unified tick() path, recompute demoted to oracle + config-gated fallback"
---

# SPEC-24 S1 — Delta-Incremental Rule Retraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the retraction-tick `recompute_rule_closure` in `crates/incremental/src/circuit.rs` with a genuinely incremental fixpoint: negative multiplicities flow through the same bilinear operators as positive ones, an incremental `distinct` (per-derived-row cumulative-weight trace) sits at the fixpoint boundary, and a retraction tick costs O(affected consequences), not O(store). Tracking issue: [#210](https://github.com/sunstoneinstitute/horndb/issues/210).

**Architecture:** One unified `tick()` path (no insertion/retraction regime fork). The circuit maintains (a) `rule_weights: BTreeMap<TripleId, BTreeMap<RuleId, i64>>` — for each row the rules derive, the number of one-step derivations per rule over the current rule-input extent (this is the DBSP incremental-distinct trace); (b) `extent: Zset<TripleId>` — the set-semantics extent rules join against, invariant `t ∈ extent ⟺ asserted_base.get(t) > 0 ∨ derived_base.get(t) > 0` (this is the shared z⁻¹ integrated input trace); (c) per-plan integrated intermediates inside `NaryPlan` (the per-level z⁻¹ state). The Stage-1 recompute stays as differential-test oracle and config-gated debug fallback.

**Tech Stack:** Rust 1.90, `proptest`, `criterion`, existing `horndb-metrics`. All work in `crates/incremental/` plus docs.

---

## Design (read this before any task)

### Why the old path is slow

Any tick containing a retraction runs `recompute_rule_closure()` — a from-scratch
set-semantics fixpoint over the whole post-delta base. One retracted triple on a
warm store costs a full rematerialization.

### The DBSP construction

Naïve Z-set accumulation diverges on cyclic recursive rules because a derived
row's multiplicity counts derivation *paths* (unbounded on cycles). DBSP's fix:
put an incremental `distinct` at the fixpoint boundary. Each round's delta is
normalized to set semantics before it feeds the next round, so the recursion
only ever sees multiplicities in {0, 1} and the tracked weight of a row is its
count of **one-step** derivations from the current set-semantics extent — finite
and stable (bounded by join fan-in, not path counts).

Incremental `distinct` per key: maintain the cumulative pre-distinct weight;
emit `+1` only when a key's total crosses 0 → positive, `-1` only on
positive → 0.

### Circuit state invariants (the heart of the change)

Let `A(t) = asserted_base.get(t) > 0`, `C(t) = closure_support.contains(t)`,
`W(t) = Σ_r rule_weights[t][r]` (0 when absent).

1. **Extent**: `t ∈ extent ⟺ A(t) ∨ derived_base.get(t) > 0`. Every weighted
   row is covered: `W(t) > 0 ⟹ A(t) ∨ t ∈ derived_base` (if neither held, the
   transition logic materializes it). So `extent` ≡ the presence set the old
   `combined_base()` computed — but maintained incrementally, never rebuilt.
2. **Weights**: at tick end, `W(t)` equals the exact number of one-step
   derivations of `t` summed over all plans, evaluated over the final `extent`.
   Maintained by folding each round's exact bilinear delta into the trace.
3. **Attribution**: `rule_attr` keys are exactly the *rule-materialized* rows
   (rows present in `derived_base` because a rule owns them). `t ∈ rule_attr ⟹
   W(t) > 0 ∧ derived_base.get(t) > 0`. A row derivable while already
   asserted/closure-covered records weights but no attr (matches the Stage-1
   "newly present" filter). `closure_support ⊆ derived_base` (unchanged).

### AMENDED 2026-07-19: deletion needs well-founded support (two-phase retraction)

The first cut of this design used one flat set of distinct transitions for both
signs: a row lives iff its cumulative one-step weight is positive. That is
**unsound on cyclic recursion**: the weight counts one-step derivations over an
extent that includes the row itself and rows it transitively supports, so it
cannot distinguish well-founded from cyclic support (the classic Gupta–Mumick
counting limitation). Minimal counterexample: with `3 SC 3` asserted, cax-sco
derives `(5,TYPE,3)` *from itself* (`x TYPE c ∧ c SC c → x TYPE c`), so after
retracting the asserted `(5,TYPE,3)` its weight stays 1 and the row wrongly
survives; a mutual SC cycle shows the same without self-loops. Positive-side
reasoning is unaffected (on insertion, every cascade step derives from rows
genuinely present, so the least fixpoint is preserved).

The deletion side is therefore **two-phase DRed-with-weights**, run inside the
one unified fixpoint:

- **R1 — overdelete cascade.** Seed with the tick's negative extent changes
  (retracted base rows whose presence drops, closure withdrawals). Per round,
  feed the negative delta through `apply_delta_stateful` as usual and fold the
  raw weight decrements. Every row that **loses ≥ 1 one-step derivation** (a
  negative raw contribution) becomes a *candidate*: provisionally remove it
  from the extent (only rows not asserted-covered and not closure-covered are
  removable) and cascade its removal in the next round. Rows never touched by
  a decrement are untouchable — this is what keeps the cascade proportional to
  the genuinely affected set.
- **R2 — re-derive.** After R1 converges, weights are exact one-step counts
  over the surviving extent, and every survivor is well-founded by induction.
  Run the positive fixpoint seeded with (a) this tick's positive extent
  changes and (b) every candidate whose weight over the surviving extent is
  positive — such a candidate has support from survivors and is re-added
  (extent `+1`), cascading normally. A candidate supported only through the
  deleted/cyclic region ends R2 with weight 0 and stays dead. Cyclic-only
  support can never re-add a row because R2 grows from well-founded rows only.
- **Net publishing.** R1/R2 mutate extent/weights freely but buffer rule-row
  materialization changes; after R2, compare each touched row's final state
  against its pre-tick state and publish only the net `RuleInferred` events
  (withdrawn = overdeleted and not re-added, with the closure-owned silent-
  lapse rule below; added = newly materialized). An overdelete-then-re-add
  therefore produces **no** feed transient.

### Distinct transitions (net, after R2; processed per touched row, key order)

- Newly rule-derivable (`W: 0 → positive` across the tick) and `¬A(t)` and
  `derived_base.get(t) == 0`: **materialize** — `derived_base += 1`,
  `rule_attr[t] = first rule-id (BTreeMap order) with positive weight`, publish
  `RuleInferred(rid) +1`.
- Newly rule-derivable, otherwise (asserted- or closure-covered): record
  weights only. No attr, no feed, no extent change beyond what A/C already
  give it.
- No longer rule-derivable (`W: positive → 0` across the tick), `t ∈
  rule_attr`: `rid = rule_attr.remove(t)`. If `C(t)`: the closure still owns
  the row — rule ownership lapses silently (no feed, no derived_base change).
  Else: zero the `derived_base` row, publish `RuleInferred(rid) -1`.
- No longer rule-derivable, `t ∉ rule_attr`: drop the weight entry; nothing
  else (the row was asserted/closure-covered; its presence is unchanged).
- A candidate that was materialized pre-tick and is re-added by R2 keeps its
  row; recompute its attr from the surviving weights (silent change is fine).
- Drop `rule_weights` entries whose total reaches 0; prune zero per-rule
  entries.

### Asserted-presence transitions (during drain)

- `A: 0 → 1` — extent gains `t` unless already present via `derived_base`.
  (A row already rule-materialized keeps its derived row — same as the Stage-1
  insertion path.)
- `A: 1 → 0` with `W(t) > 0` and `t ∉ rule_attr` and `¬C(t)`: do **not**
  eagerly materialize (that was the unsound shortcut — the weight may be
  cyclic-only). The row becomes an R1 candidate like any other; if R2 re-adds
  it, the net publish materializes it (`derived_base += 1`, attr, `RuleInferred
  +1`) — same observable outcome as the old recompute when support is
  well-founded, and a correct withdrawal when it is not.
- `A: 1 → 0` with `W(t) == 0`: extent loses `t` unless `derived_base` still has
  it (closure promote may re-add it moments later — the init-delta Zset add/
  cancel handles the net-zero case automatically).

### One tick, one path

```
tick():
  1. Drain asserted records → asserted_base; build asserted_delta (and the
     positive/negative closure-input deltas, unchanged code); process
     asserted-presence transitions, noting extent changes into init_delta.
  2. Closure retract pass (only when the tick retracts; existing logic) —
     every derived_base / closure_support mutation notes its extent change
     into init_delta.
  3. Closure insertion pass (ALWAYS here, before the rule fixpoint — the
     ordering the retraction regime already used; presence changes flow into
     init_delta so closure-derived edges feed rule bodies in the same tick,
     on every tick kind).
  4. Rule fixpoint, two phases (see the AMENDED section above):
       R1 overdelete: rounds over the negative deltas —
         raw_p   = plan_p.apply_delta_stateful(extent /* pre-round */, round_delta)
         extent += round_delta
         weight decrements; rows losing ≥1 derivation → candidates →
         provisional removals → next round
       R2 re-derive: rounds over the positive deltas + re-addable candidates
         (same call shape, positive sign), until empty.
       Then the NET distinct transitions publish. MAX_ROUNDS = 4096 with a
       panic per phase (matches the old recompute guard; never a silent
       break). A tick with no negative deltas skips R1 entirely — the
       insertion fast path is R2 alone with per-round publishing (no
       buffering needed when nothing was overdeleted).
  5. Snapshot-cache invalidation + metrics (unchanged shape).
```

Key discipline: `apply_delta` implementations use the old-old convention
(`Δ(A⋈B) = ΔA⋈B_old + A_old⋈ΔB + ΔA⋈ΔB`), so the extent passed to plans must be
the **pre-round** extent, and `round_delta` folds in only after all plans ran.
All plans in a round see the same frozen extent — no mid-round mutation.

The extent-change bookkeeping helper (no mutation of `extent` itself; the
fixpoint folds deltas at round boundaries):

```rust
/// Record into `delta` the extent-membership change for `t` implied by the
/// bases, WITHOUT mutating `extent`.
fn note_presence_change(&self, t: TripleId, delta: &mut Zset<TripleId>) {
    let now = self.asserted_base.get(&t) > 0 || self.derived_base.get(&t) > 0;
    let pending = self.extent.get(&t) + delta.get(&t) > 0;
    if now && !pending {
        delta.add(t, 1);
    } else if !now && pending {
        delta.add(t, -1);
    }
}
```

Every `asserted_base`/`derived_base` mutation outside the fixpoint's own
transition code is followed by `note_presence_change` into the pending delta.

### Per-plan integrated traces (`NaryPlan`)

The stateless `NaryPlan::apply_delta` recomputes every intermediate via
`apply_full` on each call — O(extent²) per tick for multi-join plans. New
stateful variant holds the per-level intermediate extents (the z⁻¹ state) and
updates them by folding each level's delta:

```rust
pub struct NaryPlan {
    joins: Vec<Box<dyn BilinearRule>>,
    /// Integrated left-input intermediates for joins[1..] (z⁻¹ state).
    /// None until the first stateful call (lazy cold-start from the extent
    /// passed to that call). intermediates[i] is the left input of joins[i+1].
    state: Option<Vec<Zset<TripleId>>>,
}
```

`apply_delta_stateful(&mut self, base: &Zset<TripleId>, delta: &Zset<TripleId>)
-> Zset<TripleId>`: level 0 is `joins[0].apply_delta(base, base, delta, delta)`
(no stored state — both inputs are the shared extent); level `i ≥ 1` is
`joins[i].apply_delta(&intermediates[i-1], base, &d_prev, delta)`, then
`intermediates[i-1].add_assign(&d_prev)` (update **after** use — the delta rule
needs the pre-round intermediate). `reset_state()` clears to `None` (used by the
fallback resync). The stateless `apply_delta` stays (tests and the differential
oracle fixture use it); `BilinearRule` is untouched (SPEC-04 seam).

### Recompute demoted: oracle + config-gated fallback

- `recompute_rule_closure` stays private; a `#[doc(hidden)] pub fn
  oracle_rule_closure(&self)` wrapper exposes it to integration tests.
- `Circuit::new_with_recompute_fallback()` constructs a circuit whose
  retraction-containing ticks run the Stage-1 regime (closure passes +
  recompute + diff). After such a tick the incremental state is resynced from
  scratch (`extent` rebuilt from base presence; `rule_weights` rebuilt via each
  plan's `apply_full` over the extent, which yields exactly the one-step
  weights; plan states reset) so insertion ticks — which always use the unified
  path — stay correct. The resync is O(store); acceptable for a debug fallback.
- `#[doc(hidden)] pub fn debug_validate(&self)` checks all three invariants by
  brute force (rebuild presence, recompute weights via `apply_full`, check
  attr/closure subset rules). Extended differential tests call it after every
  tick.

### Expected behavioral divergences from Stage 1 (all justified)

These are visible only in feed traffic or internal maps, never in the
materialized store (the differential gate is union-presence + all-derived-
multiplicity-1, which is preserved):

1. A row that was rule-derived and *later also asserted* used to lose its
   `derived_base` row (with a spurious `RuleInferred -1` feed record) on the
   next retraction tick, because the recompute seeded it as asserted. The
   incremental path keeps the derived row and publishes nothing. Saner; store
   state identical.
2. Closure-owned rows used to lose their `rule_attr` entry on any retraction
   tick (recompute seeds `closure_support`, so they never got attribution);
   the incremental path keeps attribution while the rule's weight is positive.
   Consequence: a later closure withdrawal of such a row correctly keeps the
   materialization (rule still owns it) instead of a transient withdraw/re-add.
3. Closure-derived edges now feed rule bodies **in the same tick** on every
   tick kind (the closure insertion pass moved before the rule fixpoint —
   the ordering retraction ticks already used). Strictly more complete; part
   of S1's "one path" and narrows SPEC-24 S8 to rule→closure feedback.
4. Feed intra-tick ordering: closure records now precede rule records on
   insertion ticks; on retraction ticks rule events are published as one net
   block after R2 (an overdeleted-then-re-added row produces no transient,
   where the old recompute-diff could publish spurious withdraw/re-add
   pairs).

Tests that pin the old quirks get updated with a comment referencing this plan
section. Tests asserting final stores, presence sets, or per-key feed nets must
pass unchanged.

### File map

- Modify: `crates/incremental/src/operator.rs` — stateful `NaryPlan`.
- Modify: `crates/incremental/src/circuit.rs` — unified tick, weight trace,
  extent, transitions, fallback, oracle/validator exposure; module docs.
- Modify: `crates/incremental/src/lib.rs` — re-exports if needed.
- Create: `crates/incremental/tests/incremental_rule_retraction.rs` — new
  targeted unit/property tests for the transitions.
- Modify: `crates/incremental/tests/acceptance_differential.rs` — extended
  differential suite (coarse mixed ticks, A/B fallback, `debug_validate`).
- Create: `crates/incremental/benches/retraction_throughput.rs` + Cargo.toml
  `[[bench]]` entry.
- Modify: `crates/metrics/src/…` + `docs/metrics.md` — one new gauge.
- Modify: docs (`docs/architecture.md`, `crates/incremental/FUTURE-WORK.md`,
  `crates/incremental/AGENTS.md`, `docs/benchmarks.md`, this plan's status).

---

### Task 1: Stateful `NaryPlan` (per-level integrated traces)

**Files:**
- Modify: `crates/incremental/src/operator.rs`
- Test: `crates/incremental/tests/nary_plan.rs` (extend)

- [x] **Step 1: Write the failing tests** — in `tests/nary_plan.rs`, add:
  (a) `stateful_delta_matches_stateless_over_random_sequences`: proptest
  driving 1–20 random `(triple, ±1)` delta batches (over a small id space,
  two-level plan built from the existing test bilinears) through both
  `apply_delta` (stateless, fed the running base rebuilt each step) and
  `apply_delta_stateful` (fed the running base only as the pre-round extent);
  after each batch, fold the delta into the running base; assert the two delta
  outputs are equal each step. Guard the base to set semantics (only feed ±1
  flips of presence, mirroring how the circuit uses it).
  (b) `stateful_cold_start_matches_full`: build a base of a few rows, call
  `apply_delta_stateful` once with a delta; assert output equals
  `apply_full(base + delta) - apply_full(base)` computed via the stateless
  reference.
  (c) `reset_state_reinitializes`: after some stateful calls, call
  `reset_state()`, continue from a different base; outputs still match the
  stateless reference.
- [x] **Step 2: Run tests, verify they fail** — `cargo nextest run -p
  horndb-incremental nary_plan` → compile error (`apply_delta_stateful`
  undefined).
- [x] **Step 3: Implement** — add `state: Option<Vec<Zset<TripleId>>>` to
  `NaryPlan` (both constructors set `None`), `apply_delta_stateful` and
  `reset_state` per the design section. Lazy init: when `state` is `None`,
  build `intermediates[i]` by the same left fold `apply_full` uses, from the
  passed `base` (which is the pre-round extent). Keep `apply_delta`/`apply_full`
  byte-identical.
- [x] **Step 4: Run tests, verify pass** — `cargo nextest run -p
  horndb-incremental nary_plan`.
- [x] **Step 5: Commit** — `feat(incremental): stateful NaryPlan with per-level
  integrated traces (SPEC-24 S1, #210)`.

### Task 2: Unified tick with weight-trace incremental distinct

**Files:**
- Modify: `crates/incremental/src/circuit.rs`
- Test: existing suite must pass; targeted new tests come in Task 3.

- [x] **Step 1: Add state** — `rule_weights`, `extent`, `recompute_fallback:
  bool` fields; `new()` keeps `recompute_fallback = false`;
  `new_with_recompute_fallback()` added. Add `note_presence_change` (code in
  design section), `base_presence(&self, t) -> bool`, and small private
  helpers: `total_weight(&self, t) -> i64`, `first_attr_rule(&self, t) ->
  Option<RuleId>` (lowest `RuleId` with positive weight).
- [x] **Step 2: Restructure `tick()`** per the design's five phases. The
  closure retract pass and closure insertion pass keep their existing logic
  but (a) the insertion pass always runs before the rule fixpoint, (b) every
  `derived_base`/`closure_support` mutation in both passes calls
  `note_presence_change` into `init_delta`, and (c) the passes' dedup checks
  read `base_presence` instead of a `combined_base` snapshot. Drain-phase
  asserted transitions per the design. Then the fixpoint loop: per round,
  collect each plan's `apply_delta_stateful(&self.extent, &round_delta)` (take
  `plans` out via `mem::take` — same borrow dance as today), fold `round_delta`
  into `extent`, merge per-plan raw deltas into `BTreeMap<TripleId,
  Vec<(RuleId, i64)>>`, process distinct transitions in key order building
  `next_delta`. `rounds_run` = fixpoint rounds; keep the existing metrics
  block and `TickReport` shape.
- [x] **Step 3: Fallback path** — when `recompute_fallback && has_retraction`,
  run the Stage-1 regime (the current retraction block, kept as a private
  method) then `resync_incremental_state()`: rebuild `extent` from base
  presence, rebuild `rule_weights` from each plan's `apply_full(&extent)`
  (skip rows seeded asserted/closure? No — record raw weights for every output
  key; the invariant is weight = one-step derivation count regardless of
  coverage), reset every plan's state, and recompute `rule_attr` consistency:
  keep existing `rule_attr` as produced by the recompute diff (it is the
  authoritative post-recompute attribution) but drop entries whose weight is
  now zero and add entries for materialized derived rows the recompute added.
  (Simplest correct resync: after the old block ran, set `rule_attr` from the
  recompute result as today, then rebuild `extent` and `rule_weights`.)
- [x] **Step 4: Delete the old insertion-regime block** (the unified path
  replaces it); `recompute_rule_closure` stays. Update `circuit.rs` module docs
  (top comment) to describe the unified path; keep it short per repo style.
- [x] **Step 5: Full crate suite** — `cargo nextest run -p horndb-incremental`.
  Existing tests that pin replaced transients (design section "Expected
  behavioral divergences") get updated **with a comment** citing this plan.
  Every store-state/final-presence assertion must pass unmodified.
- [x] **Step 6: Commit** — `feat(incremental): unified tick with incremental
  distinct — delta-incremental rule retraction (SPEC-24 S1, #210)`.

### Task 3: Oracle + validator exposure, targeted transition tests

**Files:**
- Modify: `crates/incremental/src/circuit.rs`
- Create: `crates/incremental/tests/incremental_rule_retraction.rs`

- [x] **Step 1:** Add `#[doc(hidden)] pub fn oracle_rule_closure(&self) ->
  BTreeMap<TripleId, RuleId>` (wraps `recompute_rule_closure().0`) and
  `#[doc(hidden)] pub fn debug_validate(&self)` (panics with a descriptive
  message on any invariant violation; checks: extent ≡ presence(asserted ∪
  derived); `rule_weights` ≡ Σ_p `plan.apply_full(&extent)` per key, using
  fresh stateless plans is impossible — instead compare against
  `NaryPlan::apply_full` which is `&self`; `rule_attr` keys have positive
  weight, positive `derived_base` row, and attributed rule has positive
  weight; `closure_support ⊆ derived_base` keys).
- [x] **Step 2: Targeted tests** (`tests/incremental_rule_retraction.rs`,
  using `fixtures::synthetic_rules`): chain retraction cascade (SC chain
  a⊑b⊑c⊑d, retract middle edge, assert exact surviving closure), cyclic SC
  (a⊑b, b⊑a plus c: the divergence-trap case — insert, tick, retract one
  edge, tick; assert convergence and exact store), diamond re-derivation
  (row derivable two ways, retract one support, row survives with weight 1),
  re-assert round-trip (retract then re-assert across ticks restores the
  store), mixed single tick (retract + insert replacement in one tick),
  assert-then-derive vs derive-then-assert ownership transitions (retract the
  asserted copy, row survives as rule-materialized with a `RuleInferred +1`
  feed record), duplicate asserts / over-retraction (multiplicity ≥ 2 and
  negative asserted rows behave per presence boundary). Call
  `debug_validate()` after every tick in every test.
- [x] **Step 3:** Run — `cargo nextest run -p horndb-incremental
  incremental_rule_retraction`.
- [x] **Step 4: Commit** — `test(incremental): transition + divergence-trap
  coverage for incremental retraction (#210)`.

### Task 4: Extended differential suite (the acceptance gate)

**Files:**
- Modify: `crates/incremental/tests/acceptance_differential.rs`

- [x] **Step 1:** Extend with three proptests (keep the existing three):
  (a) `coarse_mixed_ticks_match_full_rematerialize`: batches of 1–8 random
  ops (insert or presence-guarded retract) per tick, 1–6 ticks; after each
  tick `debug_validate()` + `check_equivalence`.
  (b) `incremental_matches_recompute_fallback`: drive the identical op
  sequence through a `Circuit::new()` and a
  `Circuit::new_with_recompute_fallback()`; after every tick assert equal
  union-presence sets and all-derived-multiplicity-1 on both (raw
  `derived_base` key sets may differ per the documented divergences —
  compare union presence only).
  (c) `unguarded_ops_keep_invariants`: fully random ops (duplicate asserts,
  over-retractions allowed), coarse ticks; assert `debug_validate()` passes
  and the union presence equals `full_rematerialize(presence(asserted))`
  — build the reference from the positive-presence projection of
  `asserted_base`.
- [x] **Step 2:** Run — `cargo nextest run -p horndb-incremental
  acceptance_differential`. Fix what falls out (this suite is the real gate;
  budget debugging time here).
- [x] **Step 3: Commit** — `test(incremental): extend differential suite —
  coarse mixed ticks, fallback A/B, invariant validation (#210)`.

### Task 5: Closure-interplay reconciliation

**Files:**
- Modify (as needed): `crates/incremental/tests/closure_retraction.rs`,
  `tests/retraction_closure.rs`, `tests/closure_deltas.rs`,
  `tests/closure_deltas_differential.rs`, `tests/acceptance_change_feed.rs`,
  `tests/change_feed.rs`, `tests/metrics.rs`, `tests/snapshot.rs`,
  `tests/circuit_tick.rs`, `tests/retraction.rs`

- [x] **Step 1:** Run the full crate suite: `cargo nextest run -p
  horndb-incremental`. For each failure, classify: (i) real bug in the new
  path → fix the code; (ii) test pins a replaced transient/ordering per the
  design's divergence list → update the test with a comment citing
  `PLAN-24-01` and the divergence number. Do not weaken any store-state or
  net-per-key assertion.
- [x] **Step 2:** Verify the two documented mixed-tick closure pins:
  `mixed_tick_replacement_path_final_state_correct` (final state must stay
  correct; the transient may remain — net-delta feed is S3, not this task)
  and `mixed_tick_insert_replacement_path_keeps_rule_consequence`.
- [x] **Step 3:** Full workspace check: `cargo nextest run` (production
  crates) — no cross-crate regressions expected (no crate depends on
  incremental; this is a sanity pass).
- [x] **Step 4: Commit** — `test(incremental): reconcile closure-interplay
  suites with the unified tick (#210)`.

### Task 6: Retraction bench + insertion-regression guard

**Files:**
- Create: `crates/incremental/benches/retraction_throughput.rs`
- Modify: `crates/incremental/Cargo.toml` (add `[[bench]] name =
  "retraction_throughput" harness = false`)

- [x] **Step 1:** Bench design: build a warm circuit (SC-transitive plan from
  a local copy of the bench bilinear; chain of N SC edges plus N TYPE facts
  fanned across the chain so the rule set has real consequences), then
  `b.iter` a steady-state small-delta cycle: retract one mid-chain edge,
  `tick()`, re-assert it, `tick()`. Parameterize N ∈ {64, 128, 256}. Two
  benchmark groups: `retract_small_delta/incremental` (default circuit) and
  `retract_small_delta/recompute_fallback`
  (`Circuit::new_with_recompute_fallback()`). The ≥10× acceptance ratio is
  read off the criterion report (recompute vs incremental at the same N).
  AMENDED during execution: the cut is the interior edge at position N−4,
  not the exact middle. In a bare chain a middle cut withdraws ~half the
  closure — a bulk delta, not the small-delta steady state the gate
  measures (and there the delta path is measurably no faster than
  recompute). The N−4 edge still cascades real work through both rules
  (~5N of ~N² rows) while keeping the delta small; rationale is in the
  bench's module doc. A cax-sco bilinear joins the plan's TYPE facts so
  they have real consequences (with SC-transitivity alone they are inert).
- [x] **Step 2:** Local smoke only (laptop): `cargo bench -p
  horndb-incremental --bench retraction_throughput -- --quick` and `cargo
  bench -p horndb-incremental --bench insert_throughput -- --quick`; sanity
  that incremental ≥10× recompute at N=256 and insert numbers are within
  noise of `main`. The recorded numbers land from hornbench in Task 7.
  Laptop `--quick` ratios (recompute ÷ incremental): 5.6× at N=64, 12.9×
  at N=128, 20.9× at N=256 — gate holds at N=256. Insert bench vs `main`
  (same machine, separate target dirs): branch is 1.6–3.9× *faster*
  (15.7 µs / 2.90 ms / 36.5 ms vs main's 25.3 µs / 8.01 ms / 143 ms) —
  no regression.
- [x] **Step 3: Commit** — `bench(incremental): retraction small-delta A/B
  bench — incremental vs recompute fallback (#210)`.

### Task 7: Metrics + docs sync

**Files:**
- Modify: `crates/metrics/src/…` (incremental section), `docs/metrics.md`
- Modify: `docs/architecture.md`, `crates/incremental/FUTURE-WORK.md`,
  `crates/incremental/AGENTS.md`, `docs/benchmarks.md`, this plan (status)

- [x] **Step 1:** Add gauge `horndb_incremental_distinct_trace_keys` (current
  `rule_weights` key count, set at the end of every tick) following the
  existing incremental metrics pattern; add its row to `docs/metrics.md` in
  the same commit.
- [x] **Step 2:** Docs: add a short precision note to SPEC-24 §S1 (the flat
  cumulative-weight crossing rule is unsound under deletion on cyclic
  recursion; deletion runs as an overdelete/re-derive two-phase fixpoint
  driven by the same weight trace — pointer to this plan's AMENDED section);
  flip the SPEC-24 S1 row in `docs/architecture.md`
  (planned → implemented); update `FUTURE-WORK.md` F6 "Still Stage 2"
  rule-path paragraph (delivered, pointer to this plan) leaving the
  closure-path S2 items; refresh `crates/incremental/AGENTS.md` status
  sentence; run benches on hornbench (`ssh hornbench`, repo at `~/src/horndb`,
  rsync the branch) and record retraction A/B + insert_throughput rows in
  `docs/benchmarks.md` with env note; flip this plan to `status: executed`.
- [x] **Step 3:** Full verification (Phase 3 of /next-task): `cargo fmt --all`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run
  --workspace`.
- [x] **Step 4: Commit** — `docs(incremental): S1 delta-incremental retraction
  — metrics row, architecture/benchmarks sync (#210)`.

---

## Self-review notes

- Spec coverage: S1's four design consequences map to Task 1 (operator
  traces), Task 2 (distinct trace + unified regime + incremental
  `rule_attr`), Task 3/4 (recompute demoted to oracle; differential suite),
  Task 6 (≥10× bench + insertion no-regression). Acceptance criterion 1's
  "warm LUBM-1000 with the full rule set" needs S4/E4 wiring (no consumers
  exist yet — SPEC-24 problem statement); the bench gate uses the synthetic
  warm store at the largest size the naïve reference joins allow, and the
  LUBM-scale rerun is deferred to S4/E4 with a note in `docs/benchmarks.md`.
- The closure path (S2) is untouched: `delete_transitive_edges` stays; only
  the *ordering* of the closure insertion pass changed (unified early run).
