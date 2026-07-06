---
status: executed
date: 2026-06-18
scope: "SPEC-07 Kleene property paths (`*` / `+`)"
---

# SPEC-07 Kleene property paths (`*` / `+`) — implementation plan

Tracking: epic #7, increment #50. Spec: `docs/specs/SPEC-07-sparql-frontend.md`
F3 + acceptance #7 (`?x rdfs:subClassOf* :Person`).

## Goal

Evaluate recursive Kleene property paths:

- `p+` (OneOrMore): transitive closure — all `(s, o)` connected by ≥1 `p`-step.
- `p*` (ZeroOrMore): reflexive-transitive closure — `p+` ∪ zero-length pairs.

The inner path `p` may itself be any path expression already supported by the
non-recursive lowering (`(p|q)+`, `^p+`, `(p/q)+`), so closure is computed over
the *one-step edge relation* the inner path denotes, not just a single
predicate.

## Architecture decision

The crate lowers paths to `Algebra` and evaluates an `Algebra`→`PhysicalPlan`
tree in the runtime. Non-recursive operators lower entirely at translate time.
Kleene closure cannot: it needs a fixpoint over data the executor holds.

So we add **one new node** at each layer:

- `Algebra::PathClosure { subject, object, edge, reflexive }`
- `PhysicalPlan::PathClosure { subject, object, edge, reflexive }`

where `edge` is the *boxed inner sub-plan* that produces the one-step relation
between two fixed hidden endpoint variables (`?pp_src`, `?pp_dst`), and
`reflexive` distinguishes `*` (true) from `+` (false).

The runtime evaluates `PathClosure` by:
1. Running `edge` once to materialise the one-step edge set `E ⊆ (node × node)`
   as `(src, dst)` term pairs (read off the two hidden endpoint vars).
2. Computing transitive closure of `E` via BFS from each source (semi-naive set
   growth). For `*`, also seed the reflexive pairs over the node set that
   appears anywhere in `E` plus, when an endpoint is ground, that endpoint.
3. Filtering/binding against the *actual* query endpoints (`subject`,
   `object`), which may be ground or variable, yielding `Bindings` over the
   visible endpoint variable(s).

The closure is computed in-runtime (a correct, bounded fixpoint). SPEC-05's
GraphBLAS backend is the eventual fast path for a *materialised* predicate; this
increment delivers correct evaluation for arbitrary inner paths and explicitly
defers GraphBLAS routing (documented as a deferral). This mirrors how prior
SPEC increments shipped: a correct increment now, the accelerated path later.

## Reflexive (`*`) node-set semantics

SPARQL `p*` binds the zero-length path to nodes **in the active graph**. Our
edge relation `E` only sees nodes touched by `p`. A strict reading also includes
graph nodes never touched by `p`. For this increment we take the documented
Stage-1 approximation already used by `zero_length_path`: the reflexive seed is
the set of nodes appearing in `E` (plus a ground endpoint if one is pinned).
This is correct for the common `subClassOf*`/`knows*` shapes the suite
exercises and for any query whose endpoints are constrained by the path itself.
Full graph-node enumeration for `*` is deferred (documented), same posture as
the existing `?` zero-length approximation.

## Steps (TDD)

1. **Algebra + plan nodes.** Add `PathClosure` to `Algebra` and `PhysicalPlan`;
   wire the 1:1 planner lowering. (compile-only.)
2. **translate_path: `OneOrMore`/`ZeroOrMore` arms.** Lower the inner path with
   two fresh hidden endpoint vars to an inner `Algebra`, wrap as
   `Algebra::PathClosure`. The outer `GraphPattern::Path` caller already
   `Project`+`Distinct`s to visible vars — keep that intact (closure output is
   already set-valued, but Distinct over the visible projection is harmless and
   handles the ground-both-endpoints existence case via the existing `Slice 1`
   branch).
3. **Runtime evaluation.** Implement `PathClosure` in `runtime.rs`: eval inner
   edge plan, BFS closure, bind to query endpoints.
4. **Tests.** End-to-end in `tests/exec_property_paths.rs`:
   - `?x knows+ ?y` (all reachable pairs),
   - `alice knows+ ?x` (forward from ground),
   - `?x knows+ dave` (backward to ground),
   - `?x knows* ?x`-style reflexive presence and `alice knows* ?x` includes
     alice herself,
   - `(knows|admires)+` alternative-under-plus,
   - `^knows+` inverse-under-plus,
   - cycle safety (admires creates alice→bob→alice cycle): closure terminates,
   - ground-both existence `ASK`-style `alice knows+ dave`.
   Replace the two `*_still_rejected` tests with real evaluation.
5. **Acceptance shape.** Add a `subClassOf*`-style test (rdfs:subClassOf chain →
   ancestors) demonstrating acceptance #7 semantics.
6. **Docs.** Flip `docs/architecture.md` SPEC-07 path-row Status; update SPEC-07
   if it tracks per-operator status; update crate INTEGRATION-NOTES if relevant.

## Verification

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p horndb-sparql`
- `cargo test -p horndb-sparql --features server`
- `cargo test --workspace`

## Deferred (documented in issue/architecture)

- GraphBLAS/SPEC-05 materialised-closure routing + selectivity-based planner
  choice (correctness ships now; acceleration later).
- Strict full-graph node-set semantics for `*` zero-length over nodes untouched
  by the path predicate.
