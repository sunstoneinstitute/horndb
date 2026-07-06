---
status: draft
date: 2026-06-27
scope: "`owlrl` `rdf:type` object index + genuine semi-naïve firing"
---

# 2026-06-27 — `owlrl` `rdf:type` object index + genuine semi-naïve firing

> Dated design spec (SPEC-04 F5-adjacent). Targets the LUBM-shaped
> materialize hotspot split across [#133](https://github.com/sunstoneinstitute/horndb/issues/133)
> (fix #1, object index) and [#134](https://github.com/sunstoneinstitute/horndb/issues/134)
> (fix #2, semi-naïve firing).
> Gates on existing benches per the harness-first rule (SPEC-00).

## Purpose

Cut the ~480–530 ms `cax-sco` / `rdf:type` materialization cost on
LUBM-shaped workloads (taxonomy depth 12, 40 k instances). Profiling
(`docs/benchmarks.md`, owlrl materialize A/B row, [#61](https://github.com/sunstoneinstitute/horndb/issues/61))
established the cost is **not** closure: the GraphBLAS closure phase is
~0.3 % of reason time. The gap is two compounding inefficiencies in the
compiled-rule + apply path. This spec proposes two ranked fixes, #1
independently shippable.

## The measured problem

Call path (confirmed firsthand, `cargo` workspace at this commit):

- Driver: `materialize_with` at `crates/owlrl/src/engine.rs:88`, looping
  semi-naïve rounds and attributing wall-clock across `PhaseTimings`
  (`engine.rs:26`): `compiled_rules` / `list_rules` / `closure_backend` /
  `apply`. The reason loop runs against `horndb-owlrl`'s own `MemStore`
  (the `Engine` façade in `crates/owlrl/src/integration.rs` wraps
  `MemStore`, **not** `crates/storage`).
- Measured split (`docs/benchmarks.md`, taxonomy d=12 / 40 k inst, 480 k
  inferred): reason ~627 ms, of which compiled-rules ~360 ms + apply
  ~200 ms; closure ~4 ms (noise).

**Root cause 1 — no object index.** `MemStore` holds only
`by_pred: FxHashMap<TermId, FxHashSet<(TermId, TermId)>>`
(`crates/owlrl/src/store.rs:66`). There is no object→subjects (or
subject→objects) index *within* a predicate partition. So
`probe(None, rdf_type, Some(c1))` (`store.rs:170`) is a linear scan of the
entire `rdf:type` partition filtered on object, on **every** call.

**Root cause 2 — naïve, not semi-naïve, within a rule.** The generated
`fire_cax_sco` is

```rust
for (c1, c2) in scan_predicate(rdfs_subClassOf):
    for x in probe(None, rdf_type, Some(c1)):     // full partition scan (RC1)
        emit (x, rdf_type, c2)
```

→ O(S · N) per round, S = closed `subClassOf` pairs, N = `rdf:type`
partition size. The `FireFn` signature is
`fn(&dyn TripleStore, &Delta) -> Delta` (`codegen/emit.rs`, §2.6 of
`crates/owlrl/AGENTS.md`), and the engine passes `&Delta::new()`
(`engine.rs:127`) — the delta argument is **ignored**. Every round
re-joins the entire store and discards known results via
`!store.contains(&head) && !out.contains(&head)`. For a depth-12
taxonomy this full O(S · N) join repeats ~12 rounds.

**Apply phase (~200 ms).** `crates/owlrl/src/delta.rs`: each `Delta`
carries `triples: FxHashSet<Triple>` + `proofs: FxHashMap<Triple,
Provenance>`, and every derived triple allocates a
`SmallVec<[Triple; 4]>` of premises (`codegen/emit.rs`, premise capacity
4). `insert_inferred` (`store.rs:199`) re-hashes into `by_pred`,
`inferred`, and `proofs`. Multiple hashes/allocs per derived triple
across fire → merge → apply.

Already done, out of scope here: F5 partition-by-class parallelism
([#39](https://github.com/sunstoneinstitute/horndb/issues/39)) in
`list_rules.rs` (covers only the hand-written list rules, **not** the
compiled `cax-sco` family); the `eq-rep-p` class-canonical opt
(`eq_rep_p_opt.rs`); the GraphBLAS closure swap.

## Proposed design (ranked)

### Fix #1 — within-partition object index on `MemStore` (do first)

Lowest risk, independently shippable, and helps **both** the compiled
rules and the existing F5 list-rule path (which still pays the linear
probe today).

Add a secondary index to `MemStore` alongside `by_pred`:

```rust
/// predicate → object → set of subjects. Mirrors `by_pred`; lets
/// `probe(None, p, Some(o))` return O(|extent|) instead of O(|partition|).
obj_index: FxHashMap<TermId, FxHashMap<TermId, FxHashSet<TermId>>>,
```

- Maintained in lockstep with `by_pred` in `assert`, `insert_inferred`,
  and `clear_inferred` — the three (and only three) mutation points in
  `store.rs`. Insertion-only Stage-1 (SPEC-04 F6) means no
  delete-from-index path beyond `clear_inferred`.
- `probe(None, p, Some(o))` becomes: look up `obj_index[p][o]`, map each
  subject to `Triple::new(s, p, o)`. The full-scan fallback stays for
  the `probe(Some(s), p, None)` / `probe(None, p, None)` shapes.
- **`TripleStore` trait is unchanged** — this is the key low-risk
  property. `probe`'s contract is identical; only `MemStore`'s impl gets
  faster. No codegen change, no `FireFn` change, no engine change.
- Turns the `cax-sco` inner loop from O(N) to O(|extent(c1)|).

Scope decision: maintain the object index for **every** predicate, not
just `rdf:type`. It is simpler (no vocab special-casing in the mutation
path), and other object-bound probes (`cax-eqc2`, `cls-*`) benefit. The
symmetric subject→objects index (`probe(Some(s), p, None)`) is a natural
extension but is **not** required for the hotspot and is deferred — add
it only if a later profile shows subject-bound probes dominating.

### Fix #2 — genuine delta-driven semi-naïve firing for the compiled rules

Compounds with #1: removes the ~12× redundant re-derivation. Higher risk
(codegen + signature + engine plumbing), do second, measure between.

Today every compiled rule re-joins the whole store each round. Genuine
semi-naïve fires the standard delta decomposition: for a rule with body
patterns B₁…Bₙ, fire n variants, each iterating the **previous round's
delta** Δ restricted to one pattern and the **full store** on the rest,
unioning the results. For `cax-sco` (body `subClassOf(c1,c2)`,
`type(x,c1)`):

```rust
// variant A: new subClassOf pairs × all types
for (c1, c2) in delta.scan_predicate(rdfs_subClassOf):
    for x in store.probe(None, rdf_type, Some(c1)):   // O(|extent|) via fix #1
        emit (x, rdf_type, c2)
// variant B: all subClassOf pairs × new types
for (c1, c2) in store.scan_predicate(rdfs_subClassOf):
    for x in delta.probe(None, rdf_type, Some(c1)):
        emit (x, rdf_type, c2)
```

Required changes:

1. **`FireFn` signature.** The flagged Stage-2 change
   (`crates/owlrl/AGENTS.md` §7, "the compiled rules are *not* yet
   parallelised — that needs a `FireFn` signature change"). The fire
   function must genuinely **consume** the delta. The `Delta` type needs
   a delta-side probe surface (`Delta::scan_predicate` /
   `Delta::probe(s, p, o)`); today `Delta` only exposes `iter` /
   `triples` / `dirty_predicates`. Add the same predicate-partitioned
   index to `Delta` that fix #1 adds to `MemStore` (or share a small
   read trait both implement, so codegen emits one body shape over
   `&dyn TripleStore` for both store and delta).
2. **Codegen.** `codegen/emit.rs` emits, per non-delegated rule, the
   n-variant delta decomposition instead of the single full-scan body.
   The first-round case (delta = all asserted triples, or a "fire
   everything" sentinel) must still produce the full closure — keep the
   round-1 naïve fire and switch to delta-driven from round 2, or seed
   round-1 delta with the asserted base.
3. **Engine plumbing.** `engine.rs` passes the **previous round's
   applied delta** (already computed as `applied` at `engine.rs:157`)
   into `rule.fire(store, &applied)` instead of `&Delta::new()`
   (`engine.rs:127`). The dirty-predicate prune (`rule_relevant`) stays
   as a coarse pre-filter on top.

This must stay **differential-equal** to today's closure — see Risks.

### Secondary opts (lower priority, mention only)

- **Cheaper delta/provenance.** Make proofs optional behind a flag —
  premises are only needed for SPEC-04 F4 proof trees. When proofs are
  off, skip the per-triple `SmallVec<[Triple; 4]>` alloc and the
  `proofs` `FxHashMap` insert in `delta.rs` / `store.rs:203`. The
  harness/conformance runs that need proofs flip the flag on.
- **Fold the per-fire `store.contains` double-lookup into apply.** The
  generated body checks `!store.contains(&head) && !out.contains(&head)`
  before inserting, then `insert_inferred` re-checks (`store.rs:200`).
  With semi-naïve firing the redundant containment filter can move
  wholly into the single apply pass.

## Risks and tradeoffs

- **Memory (fix #1).** The object index roughly doubles the per-triple
  index footprint (each (s, p, o) now lives in `by_pred` *and*
  `obj_index`). On the 40 k-instance / 480 k-inferred workload this is
  bounded and acceptable; record the resident-set delta in the bench.
  Mitigation if it bites: restrict the index to `rdf:type` (and a small
  allow-list of object-probed predicates) via vocab.
- **Codegen complexity (fix #2).** n-variant emission is materially more
  generated code than the single-loop body and a new failure surface in
  `emit.rs` / `plan.rs`. The `wildcard_predicate` rules
  (`eq-rep-s/p/o`) and `delegate = "closure"` rules must be handled
  exactly as today (the latter still return `Delta::new()`).
- **Correctness — must stay differential-equal.** The existing gates
  must stay green: `crates/owlrl/tests/closure_backend_differential.rs`
  (RuleFiring ≡ GraphBLAS), `tests/rdf_type_skew_differential.rs`
  (Auto ≡ Serial, incl. proptest), the W3C `owl2-w3c-rl` subset, and
  SPEC-04 acceptance #4 (reset → bit-identical store). A semi-naïve
  bug that drops a round's derivations is silent; the differential
  tests are the backstop, so any codegen change ships with the
  generated `cax-sco` output diffed via `show-rule cax-sco` (§6).
- **First-round completeness.** The most likely semi-naïve bug is
  under-firing round 1 (empty previous delta ⇒ nothing joins). The
  round-1 seed must be explicit and tested.

## Acceptance criteria

Harness-first: a fix is not satisfied until its bench moves and the
differential gates stay green.

1. **`closure_backend_differential` + `rdf_type_skew_differential` +
   `owl2-w3c-rl` subset + SPEC-04 acceptance #4 stay green** after each
   fix. Non-negotiable.
2. **Fix #1 — object index.** On the owlrl materialize A/B LUBM-shaped
   workload (`horndb-bench materialize --data <taxonomy.nt> --backend
   graphblas`, fixture from `scripts/bench/gen_workload.py taxonomy 12
   40000`), the `compiled_rules_ms` phase drops materially (target: the
   `cax-sco` inner loop is O(|extent|), so compiled-rules cost scales
   with closed type count, not N × S). Record before/after
   `compiled_rules_ms` and resident-set delta in `docs/benchmarks.md` (owlrl
   materialize A/B row). The existing `rdf_type_skew` bench should also
   improve (its `Serial`/`Auto` probes go O(|extent|)).
3. **Fix #2 — semi-naïve.** Round count and per-round delta size at
   fixpoint confirm redundant re-derivation is gone: total compiled-rule
   *work* (sum of inner-loop iterations) drops from ~O(rounds × S × N) to
   ~O(S × N) once. Record reason-time and the round/delta counters.
4. **Combined:** measurable progress toward the Stage-1 LUBM **3×**
   gate (`docs/benchmarks.md`, Stage-1 row). The gate need not fully close
   here, but the compiled-rule + apply share of reason time must fall.

**New measurement needed (instrument before optimizing):** extend
`engine::Stats` / `PhaseTimings` with per-round counters —
`rounds` already exists; add per-round **delta size** (already have
`applied`), cumulative **compiled-rule inner-loop iterations**, and at
fixpoint **N** (`rdf:type` partition size) vs **S** (closed `subClassOf`
pairs). Surface them through `horndb-bench materialize`'s JSON output
(alongside the existing `*_ms` fields in `crates/bench-rdfox/src/main.rs`)
so an A/B run can show the work-reduction directly, not just wall-clock.
Also split the `apply` phase into hash-insert vs proof-record so the
secondary proof-flag opt can be evaluated independently.

## Staging note

Ship **#1 first** (object index — no trait/codegen/engine change, just
`MemStore` internals + the new counters), land it, and record the
`compiled_rules_ms` drop in `docs/benchmarks.md`. **Then #2** (semi-naïve —
`FireFn` signature + codegen + engine plumbing), measuring the round/work
reduction against the #1 baseline. The two compound; measuring between
them keeps the attribution honest and lets #1 land even if #2 slips.
Secondary opts (proof flag, contains-fold) are opportunistic — pick them
up only if the apply-phase split (criterion 4 instrumentation) shows them
worth it.
