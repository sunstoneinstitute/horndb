# SPEC-06 — DBSP Incremental Maintenance

## Purpose

Maintain the materialized closure (rule-engine inferences from SPEC-04 + GraphBLAS closures from SPEC-05) under insertions and deletions to the base store, using **DBSP / differential-dataflow Z-set semantics** rather than RDFox's DRed or counting algorithms (FBF/B/F).

This is a deliberate departure from the OWL reasoning literature, justified by McSherry/Ryzhyk/Tannen PVLDB 2023 + VLDB Journal 2025 + the production track record of Materialize Inc.

## Scope

In scope:
- Z-set difference model: every triple has an integer multiplicity (typically +1 for asserted, with negative multiplicities representing retractions in flight).
- Stream of Z-set deltas through the rule operators (compiled in SPEC-04) and through the closure operators (SPEC-05).
- Bilinear / linear operator decomposition for rules whose body is conjunctive.
- A change-feed API so external systems (audit log, downstream consumers) can subscribe to derivation changes.
- Snapshot semantics: at any logical time `t`, the store represents a deterministic Z-set.

Out of scope:
- General DBSP query language — we only need the incremental-maintenance subset.
- Distributed timely-dataflow execution — Stage 3 / SPEC-09.
- MVCC for arbitrary point queries against in-flight deltas — Stage 2 deliverable, may require interface changes.

## Functional requirements

**F1. Z-set storage.** Each triple in SPEC-02 carries an implicit `+1` multiplicity. Deltas are stored as a sparse `(triple, ±1)` stream until merged into the main store at checkpoint boundaries.

**F2. Linear rule operator.** Rules with single-pattern bodies (`scm-*` rules: schema-only) are linear in their input; deltas pass through with the rule applied directly.

**F3. Bilinear rule operator.** Rules with two-pattern bodies are bilinear: `Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B`. The rule codegen in SPEC-04 must emit both the "fresh from scratch" and the "delta against current state" form.

**F4. n-ary rule operator.** Rules with bodies of arity n decompose into a tree of bilinear operators; the planner picks the decomposition to minimise intermediate Z-set size.

**F5. Closure-operator deltas.** SPEC-05's closures support incremental updates: insertion of `?a p ?b` triggers a recomputation of the reachable-set delta in `M_p*` rather than a full re-closure.

**F6. Retraction.** Insertion of `(t, -1)` removes `t` from the asserted base. Consequences are removed iff their derivation tree no longer holds — DBSP semantics give this for free without DRed's bookkeeping. (Caveat: see Risks on negation and stratification.)

**F7. Snapshot consistency.** A reader at logical time `t` sees a Z-set that is the sum of all `(triple, ±1)` records with timestamp ≤ `t`. Multiple deltas can be in flight; the reader's snapshot is stable.

**F8. Checkpoint merge.** Periodically, pending deltas are merged into the main store representation (collapsing `+1` and `-1` pairs to zero, removing zero-multiplicity rows). Checkpoint frequency is configurable; default 1 minute or 100K accumulated deltas, whichever first.

**F9. Change feed.** A subscriber API receives an ordered stream of `(triple, multiplicity, logical_time, derivation_kind)` records. `derivation_kind` ∈ {asserted, rule-inferred, closure-inferred}.

## Non-functional requirements

**NF1. Steady-state update latency.** Insert/retract of a single triple → all derived consequences visible to readers within 100 ms on the reference workstation, against an LUBM-1000-sized warm store. (This matches SPEC-04 NF3; the two specs jointly own this property.)

**NF2. Throughput.** Sustained insert throughput ≥100K triples/sec on a warm LUBM-8000 store, with full incremental maintenance running.

**NF3. Delta memory.** Pending delta size ≤5% of the main store size between checkpoints under sustained write load.

**NF4. Reader isolation.** Concurrent readers see a consistent snapshot. Readers do not block writers; writers do not block readers.

## Dependencies

- SPEC-02 (storage, snapshot semantics).
- SPEC-04 (rule codegen, both batch and delta forms).
- SPEC-05 (closure incremental update).

Optionally: differential-dataflow Rust crates (`differential-dataflow`, `timely`) — adopt directly if the API fits, otherwise reimplement the Z-set semantics we need (the surface area is small).

## Acceptance criteria

1. After bulk-loading LUBM-1000 and running SPEC-04 full materialization, inserting a single fresh `(student, type, GraduateStudent)` triple results in all derivable consequences being readable within 100 ms.
2. Sustained insert benchmark: 1M random N-Triple records inserted at ≥100K triples/sec into a warm LUBM-8000 store; query latency on a representative SPARQL SELECT stays within 2× of the no-write baseline.
3. Retraction correctness: insert 10K triples, retract them, and verify the store is bit-identical (modulo logical timestamps) to the pre-insertion snapshot.
4. Differential test vs full-rematerialization: after a sequence of insert/retract operations, the in-flight Z-set semantics produce the same triple set as running SPEC-04 from scratch.
5. Change-feed correctness: subscriber receives every committed delta in order with no gaps or duplicates under sustained write load.

## Risks and open questions

- **DBSP for OWL 2 RL is research-frontier.** DBSP papers focus on SQL-shaped workloads. The mapping from OWL 2 RL rule semantics to DBSP operators is straightforward in theory (Datalog is a well-understood DBSP target) but unproven on this rule set at this scale. This is the highest-risk spec in the project.
- **Stratified negation.** OWL 2 RL is negation-free in its bodies, so DBSP's monotone-circuit semantics apply directly. If we later add user rules with negation, DBSP's general framework still supports stratified negation, but we will need to verify on real workloads.
- **`differential-dataflow` crate adoption.** Adopting the full Frank McSherry `timely`/`differential-dataflow` stack imports a large dependency surface. Alternative: reimplement the narrow Z-set/operator subset we need (~few hundred LOC). Decision deferred to first prototyping pass.
- **MVCC for point queries.** Readers seeing in-flight deltas needs careful interface design. Stage 1 may expose only "at-checkpoint" snapshots and defer in-flight visibility to Stage 2.
- **Closure operator incrementality.** Incremental update of GraphBLAS-based closure (SPEC-05) is non-trivial when the transitive closure is dense; worst-case a single new edge can cause O(n²) new triples. This is fundamental to transitive closure, not a DBSP issue, but the engine must handle the worst case without OOM.
- **Backward chaining + incremental.** Backward-chained queries (SPEC-03 + SPEC-07) bypass the materialized closure, so they are not directly affected by DBSP. They are, however, affected by base-store deltas; tabling cache invalidation on insert/retract is an open design question.
