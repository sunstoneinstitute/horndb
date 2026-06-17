# Plan — SPEC-06 F6: correct retraction across joins (#45)

Parent epic: #6 (SPEC-06 incremental). Increment: #45.

## Problem

`Circuit::tick` (`crates/incremental/src/circuit.rs`) derives rule
consequences with a **set-semantics "newly present" filter**:

```rust
if combined_base.get(triple) == 0 && new_only.get(triple) == 0 { ... }
```

This emits a derived row only when it crosses absent→present in
`combined_base`. It is correct for monotone **insertion** but never
**retracts** a consequence whose support disappears. `retract_triple`
already records `(triple, -1)` in the log and folds it into
`asserted_base`, but no derived consequence is ever withdrawn. That is
the F6 gap (SPEC-06 acceptance #3/#4; `FUTURE-WORK.md` §F6).

## Design — recompute-and-diff on retraction-containing ticks

OWL 2 RL / recursive Datalog is **set semantics**: a consequence holds
iff at least one derivation holds. Pure derivation-count (bag) Z-set
accumulation diverges on cyclic recursive rules (e.g. transitive closure
over a cycle ⇒ unbounded path count), so the correct primitive is the
DBSP `distinct`-in-the-loop, realized here as **recompute the
set-semantics rule closure, then diff against the prior derived state**.

We keep two regimes inside `tick()`:

- **Insertion-only tick** (no record with `mult < 0`): the existing
  forward semi-naïve path is already a correct set-semantics
  *insertion* `distinct`. Leave it byte-for-byte unchanged — it carries
  the NF2 insert-throughput path and every current insertion test/bench.
- **Retraction-containing tick** (any `mult < 0`): recompute the
  rule closure over the post-delta `asserted_base` (presence = `get > 0`)
  to a fixpoint, then diff vs. the current rule-derived rows:
  - newly derivable → `+1`, publish `RuleInferred(rid)`;
  - no longer derivable → withdraw (`derived_base.add(t, -mult_now)` to
    zero it), publish a negative-multiplicity record.
  This is order-independent and correct for arbitrary `(triple, ±k)`.

### Attribution map

Maintain `rule_attr: BTreeMap<TripleId, RuleId>` on the `Circuit`,
recording the rule that derived each **rule-inferred** row (closure-
inferred rows stay out of the map). Updated in both regimes. The
retraction recompute uses it to (a) identify the prior rule-derived set
to diff against, and (b) attribute withdrawal records.

### Closure plans (F5) under retraction — OUT OF SCOPE (#45)

`ClosureRule` is insertion-only (`apply_insert_delta`). Closure-path
retraction is explicitly deferred under parent #6. The closure pass runs
on the **positive** part of the asserted delta in both regimes (filter
out negatives before handing the delta to closure plans). Rule-path
retraction never disturbs closure-inferred rows (they are absent from
`rule_attr`, so the diff leaves them alone). Document this clearly.

## Consistency with existing insertion semantics

The forward path excludes already-asserted triples from `derived_base`
(the `combined_base.get == 0` guard, where `combined_base ⊇ asserted`).
The recompute must match: rule-derived set = `{ t : rule-derivable AND
asserted_base.get(t) <= 0 }`. A triple that became asserted is therefore
withdrawn from `derived_base` on the next retraction-containing tick that
recomputes — still present in the store via `asserted_base`, so the
materialized union is unchanged.

## Tasks

### Task 1 — F6 retraction path in `Circuit::tick` (TDD)

Implement the design above. Add `rule_attr` field; factor the closure
pass to consume positive-only delta; branch `tick()` on
`has_retraction`; add `recompute_rule_closure()` returning
`BTreeMap<TripleId, RuleId>` (rule-derived, non-asserted, set
semantics).

Tests (write first, watch fail, then implement):
- `tests/retraction.rs::retract_base_withdraws_consequence`: assert
  `(0,P,1)`,`(1,P,2)` with a transitive rule → tick → `(0,P,2)` derived.
  Retract `(1,P,2)` → tick → `derived_base.get(&(0,P,2)) == 0`,
  `asserted_base.get(&(1,P,2)) == 0`. A feed subscriber sees a negative
  `RuleInferred` record for `(0,P,2)`.
- `tests/retraction.rs::insert_10k_retract_10k_bit_identical` (SPEC-06
  acceptance #3): build the 3-rule synthetic circuit; assert 10K random
  triples, tick; snapshot; retract all 10K, tick; assert
  `asserted_base` and `derived_base` are both empty (bit-identical to
  the empty pre-insertion snapshot, modulo logical timestamps).
- A multi-derivation retraction test: a consequence with two independent
  supports stays present when only one support is retracted, and is
  withdrawn when the second is retracted.

All existing insertion tests/benches must stay green unchanged.

### Task 2 — tighten acceptance #4 to multiplicity equality + retraction

Extend `tests/acceptance_differential.rs`:
- A new proptest that interleaves inserts and retracts (retracting a
  random subset of previously-asserted triples), ticking between, and
  asserts the Circuit's `(asserted ∪ derived)` equals `full_rematerialize`
  of the net asserted set — compared by **multiplicity** (every present
  key has multiplicity exactly 1; absent keys 0), not just support set.
- Update the module/`check_equivalence` doc-comment to reflect that F6
  has landed and multiplicities are now meaningful.

### Task 3 — docs sync (same PR, no `TASKS.md`)

- `crates/incremental/FUTURE-WORK.md`: move F6 from "Stage 2" to
  delivered (note the recompute-and-diff retraction regime; the
  fully-incremental retraction path and closure-path retraction remain
  the next optimization). Update the acceptance-#4 set-semantics caveat
  (now multiplicity-checked).
- `docs/architecture.md`: flip row 50 (system table) and the SPEC-06
  detail rows 239/251 — retraction (F6) `deferred` → `implemented
  (recompute-and-diff)`; overall status note. Keep closure-path
  retraction deferred.
- `crates/incremental/src/lib.rs` + `circuit.rs` module docs: drop /
  qualify the "insertion-only" framing for the rule path.

`TASKS.md` is intentionally NOT touched on this branch (handled as a
locked commit on `main` after merge, per `/next-task`).

## Gate

`cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test -p horndb-incremental` (incl. the new acceptance tests),
`cargo test --workspace`.
