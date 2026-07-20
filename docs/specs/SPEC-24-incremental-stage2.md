---
status: approved
date: 2026-07-18
scope: "SPEC-06 Stage 2 — delta-incremental retraction (rule + closure paths), change-feed net-delta + backpressure, engine wiring, DeltaLog WAL, MVCC backing of snapshots, bilinear-join runtime; refines SPEC-06, coordinates with SPEC-02 Stage 2 (E3) and SPEC-04 (E4)"
---

# SPEC-06 Stage 2 — incremental maintenance completeness

**One-line thesis:** Stage 1 proved the Z-set model correct; Stage 2 makes it
*incremental all the way down* — a retraction tick must cost what the retraction
touches, not what the store holds — and connects the circuit to the rest of the
engine (update path, WAL, storage MVCC) so the model stops being a sealed lab.

**Refines:** SPEC-06 (the standing incremental-maintenance contract — its F1–F9
and acceptance criteria stay in force; this spec adds the Stage-2 requirements
S1–S8 below). Coordinates with SPEC-02 Stage 2 / epic E3
([#187](https://github.com/sunstoneinstitute/horndb/issues/187)) for per-tuple
MVCC and the on-disk WAL, with SPEC-04 / epic E4
([#188](https://github.com/sunstoneinstitute/horndb/issues/188)) for owlrl
Z-set rule wiring and delta-form codegen, and with SPEC-23 (§5.3 `Stats` seam)
for join costing.
**Epic:** [#186](https://github.com/sunstoneinstitute/horndb/issues/186).
Successor to the Stage-1 epic
[#6](https://github.com/sunstoneinstitute/horndb/issues/6).

## Problem — what Stage 1 ships, and where it stops

`horndb-incremental` (1.6 KLOC; `crates/incremental/src/`) implements the
SPEC-06 Z-set model: every triple carries an integer multiplicity
(`Zset<TripleId>` over a `BTreeMap`, multiplicity `i64`), deltas flow through
registered rule operators (`NaryPlan` over `BilinearRule`s) and closure
operators (`ClosureRule` wrapping SPEC-05's `IncrementalClosureBackend`), a
change feed publishes every derived change, and refcounted `Snapshot` handles
give readers a stable view while writers keep ticking. Insertion is genuinely
incremental: an insertion-only `Circuit::tick` runs a semi-naïve delta fixpoint
and touches work proportional to the delta.

Retraction is **correct but not incremental**. Any tick containing a
retraction switches `Circuit::tick` into a second regime
(`crates/incremental/src/circuit.rs`):

1. a closure-retraction pass (`ClosureRule::apply_retract_delta` →
   SPEC-05 `delete_transitive_edges`), which internally **recomputes
   base-reachability over the affected source region** per retracted edge;
2. a closure-insertion pass (so mixed ticks see the post-tick closure);
3. `recompute_rule_closure()` — a **from-scratch set-semantics fixpoint over
   the whole post-delta base** (seeded from `asserted_base ∪ closure_support`),
   diffed against the prior rule-derived rows via the
   `rule_attr: BTreeMap<TripleId, RuleId>` attribution map.

So one retracted triple on a warm store costs a full rematerialization of the
rule closure. That was the deliberate Stage-1 trade (recompute-and-diff is
order-independent and dodges the divergence trap described in S1 below), and it
is what epic #186 exists to replace: the DBSP promise — the reason ADR-0008
picked Z-sets over DRed/counting in the first place — is that deletion is the
*same* algebra as insertion, with negative multiplicities flowing through the
same operators.

Beyond retraction, Stage 1 left five structural gaps, all catalogued in
`crates/incremental/FUTURE-WORK.md`:

- **The circuit has no consumers.** No crate in the workspace depends on
  `horndb-incremental` — not `sparql`, not `storage`, not `harness`. The
  public driver surface (`assert_triple` / `retract_triple` / `tick` /
  `subscribe` / `snapshot`) is exercised only by the crate's own tests and
  benches. SPEC-06's own acceptance criteria 1 and 2 (100 ms visibility and
  100K inserts/sec against warm LUBM stores) are therefore not runnable
  end-to-end today. Concrete `BilinearRule` impls exist only in test fixtures;
  production rules arrive via E4's owlrl wiring.
- **The change feed over-reports and cannot push back.** A mixed tick that
  withdraws a closure edge and re-adds it via a replacement path publishes a
  transient `ClosureInferred -1` then `+1` (net zero) and counts both in
  `derived_merged` (pinned by
  `tests/closure_retraction.rs::mixed_tick_replacement_path_final_state_correct`).
  Subscriber channels are unbounded crossbeam channels; a slow subscriber grows
  memory without limit.
- **Nothing is durable.** `DeltaLog` is an in-memory `Vec`; a crash between
  checkpoints loses pending deltas. The `drain()` interface was shaped to be
  WAL-compatible, but there is no on-disk log, no fsync policy, no replay.
  `Checkpoint::merge` exists but nothing schedules it — the SPEC-06 F8 cadence
  (1 min / 100K deltas) is unimplemented; `tick()` drains the log inline.
- **Snapshots are in-process only.** `Circuit::snapshot()` serves readers from
  a lazily built, Arc-cached presence view — amortized O(1), but the first
  acquire after a write pays one O(|asserted| + |derived|) rebuild, and the
  view lives entirely on the heap next to the circuit. SPEC-02's per-tuple
  MVCC (Stage 2, epic E3) is the intended backing; the storage tier today has
  only whole-tier copy-on-write versioning (`TierSnapshot.version: u64`) and
  **no delete path at all**.
- **The join runtime is a placeholder.** `NaryPlan` folds left-deep with no
  cost model, and every leaf reads the *same whole-base extent* (no
  per-predicate slicing). The reference joins are O(n²) nested loops in test
  fixtures. Warm-store closure seeding (`seed_closed_edges`) uses the *closed*
  extent as a conservative base, so retraction against seeded support is sound
  but may under-withdraw.

## Non-goals

- **Changing the Stage-1 contract.** SPEC-06 F1–F9, NF1–NF4, and acceptance
  1–5 stand unchanged. This spec adds requirements; it does not relax any.
- **Distributed execution.** Single-process `Circuit` throughout; the
  `timely`/`differential-dataflow` question stays deferred to Stage 3
  (SPEC-09), per FUTURE-WORK.
- **Owning the storage-side machinery.** Per-tuple visibility stamps, the
  on-disk WAL format, and the persistent dictionary belong to SPEC-02 Stage 2
  (epic E3). This spec defines the contracts the circuit needs from them
  (S5, S6) and consumes them when they exist.
- **Owning the rule surface.** Which OWL 2 RL rules exist, their delta-form
  codegen (`rules.toml` → `BilinearRule::apply_delta` bodies), and registering
  them on the circuit is SPEC-04 / epic E4 territory ("owlrl Z-set wiring").
  This spec owns the operator *runtime* those rules run on.
- **General negation.** OWL 2 RL bodies are negation-free; the incremental
  fixpoint machinery here (S1) assumes monotone rules. Stratified negation for
  user rules is future work on top of this spec, not part of it.

## Stage-2 requirements

### S1. Delta-incremental rule retraction

Replace the retraction-regime `recompute_rule_closure` with a genuinely
incremental fixpoint: negative multiplicities thread through the same bilinear
operators as positive ones (`Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B`, the
DBSP correctness theorem — McSherry/Ryzhyk/Tannen, PVLDB 2023 §3), and a
retraction tick costs O(affected derivations), not O(store).

**The divergence trap, and the DBSP answer.** Stage 1 rejected naïve Z-set
accumulation because on cyclic recursive rules the multiplicity of a derived
row counts *derivation paths*, which is unbounded on cycles — the fixpoint
never converges. DBSP's construction avoids this by placing an incremental
`distinct` (set-semantics normalization) at the fixpoint boundary: each
round's delta is normalized to set semantics before it feeds the next round,
so multiplicities stay in {0, 1} inside the recursion and convergence is
guaranteed for monotone (negation-free) programs. Incremental `distinct` is
computable per key from a maintained cumulative-weight trace: emit `+1` only
when a key's cumulative weight crosses 0 → positive, `-1` only on
positive → 0.

Design consequences:

- **Operator state.** Each registered `NaryPlan` (and each join level inside
  it) maintains integrated input traces (the DBSP `z⁻¹` delay state) so
  `apply_delta` can be driven with the true per-level `A`/`B` extents instead
  of today's whole-`combined_base`-everywhere approximation. The circuit
  maintains a per-derived-row cumulative-weight trace backing the incremental
  `distinct`. Memory: O(derived rows + operator inputs); budgeted under
  SPEC-06 NF3.
- **Attribution survives.** `rule_attr` (needed by the closure/rule dual-
  ownership logic and the change feed's `RuleInferred(RuleId)` tagging) must be
  maintained incrementally alongside the distinct trace.
- **The recompute path is demoted, not deleted.** `recompute_rule_closure`
  becomes the differential-test oracle (the same role `BackendImpl` full
  recompute plays for closure deltas): every delta-incremental result is
  pinned against it by property tests extending
  `tests/acceptance_differential.rs`, and it remains available as a debug
  fallback behind a config flag.
- **Insertion regime unifies.** Once negative deltas flow through the
  operators, the two `tick()` regimes collapse into one code path; the
  insertion-only fast path may remain as an optimization but no longer as a
  semantic fork.

**Precision note (implementation).** The flat cumulative-weight crossing rule
sketched above is unsound under deletion on cyclic recursion: a derived row
can support itself through a cycle, so its cumulative weight stays positive
after its real (base-grounded) support is gone, and the row is never
retracted. The implementation therefore runs deletion as a two-phase
overdelete / re-derive fixpoint (DRed-style: first delete everything the
retracted rows could have contributed to, then re-derive what still has
alternative support), driven by the same one-step weight trace. See
`PLAN-24-01`, AMENDED section.

### S2. Delta-incremental closure retraction + exact seeded retraction

The closure path gets the same treatment on the SPEC-05 boundary:

- **Output-sensitive deletion.** Replace per-edge affected-region
  base-reachability recompute inside `delete_transitive_edges` with a
  maintained support structure so a deletion costs O(closure delta + frontier
  touched). Candidate designs, to be settled by the implementation plan with
  bench evidence: per-pair support counts in the GraphBLAS semiring (path
  counts saturating at a cap, so cyclic densities cannot overflow), or
  per-predicate descendant/spanning structures (Italiano-style). Fully dynamic
  transitive closure has fundamental worst-case bounds — a single deleted
  edge can invalidate O(n²) pairs — so the requirement is *output
  sensitivity* (cost proportional to what actually changed plus the frontier
  inspected), not a universal speedup. The existing differential proptests
  (`tests/incremental_retraction.rs`, `tests/closure_deltas_differential.rs`)
  gate correctness.
- **Exact warm-store seeded retraction.** Add a base-seed variant
  (`seed_base_edges`) so a warm-started `TransitiveClosureRule` knows the true
  asserted base, closing the known under-withdraw hole of `seed_closed_edges`
  (sound today, exact after this). The closed-extent seeding stays available
  for callers that genuinely have only the closed set; its conservatism gets
  documented at the API.

> **Status:** delivered by `PLAN-24-02` (#211). Output-sensitive deletion is a
> support-counting decremental sweep (`DeleteStrategy::SupportCounting`, the
> default); the recompute path is retained as `DeleteStrategy::Recompute` (the
> differential oracle). Exact seeded retraction ships as `seed_base_edges`.

### S3. Change-feed net-delta reconciliation + bounded backpressure

- **Net-delta contract.** Within one tick, derived-row emissions accumulate in
  a tick-local Z-set keyed by `(triple, kind)`; at tick end, only non-zero net
  records are published (in deterministic key order — the reason `Zset` sits
  on a `BTreeMap`), and `derived_merged` counts net records. The same-tick
  closure withdraw+re-add transient disappears from the feed; SPEC-06
  acceptance 5 ("every committed delta, in order, no gaps or duplicates") is
  then interpreted over *net* per-tick deltas. Asserted records keep their
  per-record publish semantics (they are the user's own operations, in the
  user's order). The pinned test
  `mixed_tick_replacement_path_final_state_correct` flips from documenting the
  transient to asserting its absence.
- **Bounded subscribers.** `subscribe()` grows a bounded variant with an
  explicit per-subscriber lag policy: `Block` (backpressure the tick),
  `DisconnectSlow` (drop the subscriber, surface it on the
  `change_feed_subscribers` gauge plus a new drop counter), with
  `DisconnectSlow` as the default. Unbounded remains available explicitly.
  This is a breaking semantic change window that costs nothing today —
  the crate has no external subscribers yet (see S4).
  New/changed metrics land with their `docs/metrics.md` rows in the same
  commit, per the root sync rule.

### S4. Engine wiring — the circuit gets consumers

Wire `Circuit` behind the engine's update path so incremental maintenance
stops being latent:

- SPARQL Update (`INSERT DATA` / `DELETE DATA` / `DELETE/INSERT … WHERE`,
  SPEC-07) lowers each update operation to `assert_triple` / `retract_triple`
  batches plus one `tick()` per operation boundary.
- Readers reach derived rows through the `Snapshot` surface (or through the
  store once S6 lands); the change feed becomes the audit/downstream seam it
  was specified to be (SPEC-06 F9).
- Rule registration is a seam, not a bundle: the circuit exposes
  registration (`add_plan` / `add_closure_plan`) and E4's owlrl wiring
  populates it. A minimal synthetic-rule wiring lands here to prove the seam
  end-to-end; LUBM-scale runs of SPEC-06 acceptance 1–2 become possible once
  E4's rules plug in.
- Boundary with E3/E4 stated plainly: E2 owns the circuit-side API and the
  update-path integration; E4 owns which rules are registered; E3 owns where
  derived rows persist.

### S5. Durability — DeltaLog WAL + checkpoint scheduling

- **WAL seam.** `DeltaLog` gets a write-ahead-log backing behind its existing
  append/drain shape: `append` becomes a sequenced, durably-appended record
  (fsync policy configurable: per-record / per-tick / timed), `drain` pairs
  with log truncation at checkpoint, and recovery replays records since the
  last checkpoint. The on-disk format and per-predicate-partition layout
  belong to SPEC-02 Stage 2 (E3); this spec owns the log *contract* (ordering,
  atomicity of a tick's batch, replay-to-identical-state) and the crash tests
  that pin it. **Settled — ADR-0018:** one physical log (the SPEC-25 S3 WAL)
  with typed records; this contract is a thin layer over it (`Input` records
  durable on append; `TickCommit` marks the drained range at its commit
  version; recovery re-submits inputs past the last tick marker).
- **Checkpoint scheduling.** Implement the F8 cadence (configurable; default
  1 min / 100K deltas, whichever first) as an actual scheduler driving
  `Checkpoint::merge` + WAL truncation, replacing today's
  inline-drain-only behavior.

### S6. MVCC backing of snapshots

Back `Circuit::snapshot()` onto SPEC-02 per-tuple visibility once E3 provides
it:

- **Contract the backing must satisfy** (what `snapshot.rs` promises readers
  today): `contains(&TripleId)`, key-ordered `iter()`, `len`/`is_empty`, and
  `logical_time()` as an inclusive as-of token; pinned immutably across
  concurrent writes; cheap to clone.
- **Version reconciliation.** The circuit's `LogicalTime` and storage's
  whole-tier `TierSnapshot.version: u64` are independent counters today.
  **Settled — ADR-0018:** the storage commit version is the shared clock; a
  tick commits as one storage batch and adopts its commit version, so
  "snapshot at t" means the same thing in both layers with no mapping.
- **What this buys.** The O(|asserted| + |derived|) first-acquire presence
  rebuild disappears — visibility becomes a per-tuple predicate evaluated at
  scan time in storage — and point queries against partially applied in-flight
  deltas mid-tick (explicitly out of Stage-1 scope) become expressible.
  Blocked on E3's per-tuple visibility stamps and delete path; until then the
  in-process snapshot stays.

### S7. Bilinear-join runtime — cost model + real joins

- **Per-predicate leaf extents.** `NaryPlan` leaves bind to per-predicate
  slices of the base (today every leaf scans the whole extent), which is both
  a correctness prerequisite for S1's operator traces and the main constant-
  factor win.
- **Cost-based decomposition.** Replace the fixed left-deep fold with
  cost-based join-tree selection over the SPEC-23 §5.3 `Stats` seam
  (per-predicate counts/NDV) — the SPEC-06 F4 "planner picks the decomposition
  to minimise intermediate Z-set size" requirement, unimplemented in Stage 1.
- **Real join kernels.** Hash and sort-merge `BilinearRule` bodies replace the
  O(n²) nested-loop reference joins; SPEC-04's codegen emits them per rule
  shape (coordinate with E4 — the codegen lives there, the trait contract and
  the runtime live here).

### S8. Intra-tick feedback + non-transitive closure shapes

- **Closure↔rule feedback within one tick.** Today closure→rule feedback works
  on retraction ticks (the closure passes run before the rule recompute) but
  *not* within a pure insertion tick (the closure insertion pass runs after
  the rule forward pass), and rule→closure feedback (a rule-derived edge of a
  closure predicate extending the closure) never happens in-tick. Iterate the
  two passes to a joint fixpoint so a tick's outcome is
  ordering-independent — the current one-pass ordering is a documented
  approximation.
- **Non-transitive closure shapes.** `ClosureRule` today has one implementor
  (`TransitiveClosureRule`). Extend to the other SPEC-05 shapes (symmetric /
  inverse / equivalence-class closures) so closure-eligible rules beyond
  transitivity can ride the same delta path.

## Phasing

Each phase is an independently shippable increment, tracked as a sub-issue of
epic [#186](https://github.com/sunstoneinstitute/horndb/issues/186) and
harness-gated (the SPEC-01 selected subset plus this crate's differential
suites stay green throughout). Implementation plans (`PLAN-24-MM-*.md`) are
written when each increment is picked up.

1. **S1 — delta-incremental rule retraction**
   ([#210](https://github.com/sunstoneinstitute/horndb/issues/210)). The core
   DBSP bet: incremental distinct + operator traces; recompute demoted to
   oracle. Highest risk, highest value; everything else composes with it.
2. **S2 — delta-incremental closure retraction + exact seeded retraction**
   ([#211](https://github.com/sunstoneinstitute/horndb/issues/211)).
   SPEC-05 boundary work; differential proptests gate it.
3. **S3 — change-feed net-delta + bounded backpressure**
   ([#212](https://github.com/sunstoneinstitute/horndb/issues/212)). Small,
   self-contained, and best landed *before* external subscribers exist.
4. **S4 — engine wiring**
   ([#213](https://github.com/sunstoneinstitute/horndb/issues/213)). Update
   path → circuit → readers; synthetic-rule seam proof; unblocks LUBM-scale
   acceptance runs (with E4).
5. **S5 — DeltaLog WAL + checkpoint scheduling**
   ([#214](https://github.com/sunstoneinstitute/horndb/issues/214)). Contract
   + crash tests here; on-disk format arrives with E3.
6. **S6 — MVCC backing of snapshots**
   ([#215](https://github.com/sunstoneinstitute/horndb/issues/215)). Blocked
   on E3 per-tuple visibility; land the version-reconciliation design early so
   E3 builds the right thing.
7. **S7 — bilinear-join runtime**
   ([#216](https://github.com/sunstoneinstitute/horndb/issues/216)).
   Per-predicate leaves, cost model over the SPEC-23 `Stats` seam,
   hash/sort-merge kernels with E4 codegen.
8. **S8 — intra-tick joint fixpoint + non-transitive closure shapes**
   ([#217](https://github.com/sunstoneinstitute/horndb/issues/217)).
   Completeness tail; ordering-independence property tests.

Phases 1–3 are pure `horndb-incremental` work and can proceed immediately.
Phase 4 needs SPEC-07 coordination. Phases 5–6 consume E3 deliverables.
Phase 7 consumes SPEC-23 phase 3 (`Stats`) and E4 codegen. Phase 8 is
independent but lowest urgency.

## Acceptance criteria

1. **Retraction is output-sensitive (S1).** On a warm LUBM-1000 store with the
   full rule set registered, retracting one triple completes in time
   proportional to its affected consequences; a criterion bench pins the
   retraction tick at ≥10× faster than the Stage-1 recompute path on
   small-delta ticks, and the recompute-oracle differential suite (extended
   `tests/acceptance_differential.rs`, multiplicity equality over interleaved
   insert/retract) stays green.
2. **Closure deletion is output-sensitive (S2).** Same shape on the closure
   path: deletion cost scales with the closure delta, not the store;
   `tests/incremental_retraction.rs` / `tests/closure_deltas_differential.rs`
   proptests stay green; a seeded-base retraction test shows exact (not
   conservative) withdrawal.
3. **The feed reports net truth (S3).** The mixed-tick replacement-path test
   asserts zero net feed records and `derived_merged` counts net records; a
   slow-subscriber test shows the configured lag policy engaging with no
   unbounded memory growth.
4. **The circuit has real consumers (S4).** A SPARQL `DELETE DATA` against a
   store with registered rules withdraws the retracted triple's consequences,
   observable through both a fresh query and the change feed — exercised in
   the harness, not just crate tests.
5. **Crash recovery works (S5).** A kill-and-replay test (tick, crash before
   checkpoint, recover) reproduces the exact pre-crash Z-set state, bit-
   identical modulo logical timestamps.
6. **Snapshots ride storage MVCC (S6).** With E3 landed, `snapshot()` no
   longer materializes an in-process presence view (no O(n) first-acquire
   rebuild), and a pinned snapshot's reads are stable while concurrent ticks
   commit — verified under concurrent reader/writer tests.
7. **SPEC-06 acceptance 1–5 hold end-to-end.** The original criteria —
   100 ms visibility (with E4 rules), 100K/s sustained inserts, 10K
   insert/retract bit-identity, differential equivalence, gap-free feed —
   run against the *wired* engine (S4) on hornbench and pass.

## Risks and open questions

- **DBSP recursive incrementality at this rule set's scale is unproven.**
  SPEC-06 already carries this flag; S1 is where it cashes out. The
  incremental-distinct trace adds memory proportional to derived rows —
  NF3's 5% delta-memory budget needs re-validation once real traces exist.
  Mitigation: the recompute oracle never leaves the tree.
- **Fully dynamic transitive closure has no free lunch.** A single edge
  deletion can legitimately invalidate O(n²) closure pairs; S2's requirement
  is output sensitivity, and the bench gate must compare against the Stage-1
  affected-region recompute honestly (which is itself partially output-
  sensitive). It is possible the counting design loses on some densities;
  the plan must carry a keep-the-recompute fallback per predicate.
- **Feed semantics change.** Net-delta publishing reinterprets SPEC-06
  acceptance 5. Doing it before S4 creates subscribers is the entire
  mitigation — after S4, this would be a breaking change to real consumers.
- **E3 sequencing.** S5/S6 consume storage deliverables that do not exist
  yet (per-tuple visibility, delete path, WAL format). If E3 slips, S5 can
  still land the log contract against a file-backed stub; S6 cannot — it
  stays blocked. The version-reconciliation design is agreed — **ADR-0018**
  (storage commit version as the shared clock, single typed WAL).
- **Tick regime unification could regress insertion latency.** Collapsing the
  two `tick()` regimes (S1) must not slow the insertion-only fast path that
  meets NF1/NF2 today; the criterion benches (`insert_throughput`) gate the
  refactor.
- **Where does `distinct` state live under memory pressure?** The cumulative-
  weight trace is hot-path state. Whether it stays a `BTreeMap` next to
  `derived_base`, moves into storage, or becomes spillable intersects E3
  tiering — open until S1 prototyping.
