# Plan: Wire the SPEC-05 GraphBLAS closure backend into the owlrl Engine (#61)

**Status:** in progress · **Issue:** #61 (HIGH · Performance) · **Branch:** `task-61-graphblas-closure-backend`

Timing follow-up to #59. #59 made the LUBM(1) closure-count **parity** gate exact
(delta 0). What remains is the **separate** Stage-1 "within 3×" *timing* gate.
Today the owlrl `Engine` hard-wires the nested-loop `RuleFiringBackend`
(`crates/owlrl/src/backend.rs`) — "slow but obviously correct". This plan makes
the closure backend injectable, adds a `horndb-closure`-GraphBLAS-backed impl,
and profiles LUBM(1) materialize to attribute cost and report how much of the
gap the swap closes.

## Constraints (gates)

1. **Parity:** the GraphBLAS backend must produce a materialized triple **set**
   identical to `RuleFiringBackend` on every input. This is the acceptance gate
   and the core risk.
2. **No forced GraphBLAS dependency** on default `horndb-owlrl` builds: the new
   backend lives behind a cargo feature so plain `cargo build -p horndb-owlrl`
   stays GraphBLAS-free and `Engine::new()` keeps firing `RuleFiringBackend`
   (zero behavior change for existing callers / feature-off builds).
3. **Harness-first:** existing owlrl tests + the W3C subset stay green.

## What `RuleFiringBackend::close` actually computes (the parity target)

Per `crates/owlrl/src/backend.rs`, `close` loops to its own fixpoint over the
current store and returns a `Delta` of:

- `scm-sco`: **strict** transitive closure of `rdfs:subClassOf` edges (NO
  reflexive `?c ⊑ ?c` — that comes from a separate compiled rule, not the
  backend).
- `scm-spo`: strict transitive closure of `rdfs:subPropertyOf` edges.
- `eq-sym` + `eq-trans`: symmetry then transitivity of `owl:sameAs`, iterated to
  fixpoint. Combined, this is the **strict transitive closure of the symmetrized
  `owl:sameAs` matrix** `M ∨ Mᵀ` — which includes the diagonal `(a,a)` for any
  element in a class of size ≥ 2 (a↔b ⇒ a→b→a), matching the nested-loop result.
- `prp-trp`: for each predicate `p` with `(p rdf:type owl:TransitiveProperty)`,
  strict transitive closure of `p`'s edges.

It does **not** compute `eq-ref` (reflexive `?x sameAs ?x` for all `x`). The
GraphBLAS backend must match this exactly — no reflexive subclass/subproperty,
no `eq-ref`.

> **Why we must NOT reuse `horndb_closure::sink::BackendImpl`:** its
> `close_subclass`/`close_subproperty` use `reflexive_transitive_closure` (adds
> the identity) — a parity break — and its `add_sameas` only unions into a
> `EquivClasses` union-find and emits **no triples**, whereas the owlrl engine
> needs the `eq-*` triples materialized into the store. So the bridge uses the
> lower-level primitives directly: `grb::BoolMatrix`, `dense_id::DenseIdMap`,
> and `closure::transitive::transitive_closure` (the strict one).

## Design

### 1. owlrl → closure dependency, feature-gated

`crates/owlrl/Cargo.toml`:

```toml
[features]
default = []
graphblas-backend = ["dep:horndb-closure"]

[dependencies]
horndb-closure = { path = "../closure", optional = true }
```

### 2. New backend impl: `crates/owlrl/src/graphblas_backend.rs` (`#[cfg(feature = "graphblas-backend")]`)

`pub struct GraphBlasBackend` implementing `crate::backend::ClosureBackend`
(`fn close(&mut self, store: &dyn TripleStore) -> Delta`). Per call:

- Collect edges per closure family from the store (`scan_predicate`).
- For `sco`, `spo`, and each transitive property: build a `BoolMatrix` via a
  `DenseIdMap` over the family's `TermId`s (TermId(u64) → DictId(u64) is the
  identity newtype), run **strict** `transitive_closure`, map dense edges back
  to `TermId`, emit each closure triple not already in `store` with the right
  `rule_id`.
- For `owl:sameAs`: build the matrix from edges **plus their reverses** (`M ∨
  Mᵀ`), run strict `transitive_closure`, emit. `rule_id` = `eq-sym` for an edge
  whose reverse is asserted-or-derived-symmetric and `eq-trans` otherwise (best
  effort; provenance `rule_id` is metadata — only the triple *set* is gated).
- Provenance `premises`: best-effort (parity gate is the triple set, not the
  proof). Record the direct contributing pair where cheap, else minimal.

GraphBLAS is initialized via `horndb_closure::grb::init_once()` in the
constructor (idempotent).

### 3. Injectable `Engine` (`crates/owlrl/src/integration.rs`)

```rust
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq)]
pub enum BackendChoice {
    #[default]
    RuleFiring,
    #[cfg(feature = "graphblas-backend")]
    GraphBlas,
}
```

`Engine` stores `backend: BackendChoice`. Add `Engine::with_backend(BackendChoice)`
(and keep `Engine::new()` defaulting to `RuleFiring`). `load()` matches on the
choice and passes the concrete backend to `reset_and_materialize` — no `dyn`,
generics preserved. The `RuleFiringBackend::new()` at the current
`integration.rs:152` becomes the `RuleFiring` arm.

### 4. Parity test: `crates/owlrl/tests/closure_backend_differential.rs` (`#[cfg(feature = "graphblas-backend")]`)

For a corpus — synthetic subclass/subproperty chains, `owl:sameAs` classes of
varying size, transitive-property graphs, plus a couple of representative W3C
fixtures already used by other owlrl tests — load via
`Engine::with_backend(RuleFiring)` and `Engine::with_backend(GraphBlas)` and
assert the materialized triple sets are byte-for-byte identical
(`all_triples()` equality). This extends the SPEC-05 differential-equality
discipline to the owlrl path (issue acceptance #3).

### 5. Profiling (issue acceptance #2)

- Add opt-in per-phase timing to `materialize_with`: sum wall-clock spent in
  (a) compiled rules, (b) list rules, (c) `backend.close`. Surface via `Stats`
  (populated only when an `opts` flag / env var is set, so the hot path is
  untouched by default) and expose through the `Engine`.
- Extend `crates/bench-rdfox` `materialize` with `--backend rulefiring|graphblas`
  (enabling owlrl's `graphblas-backend` feature for the binary) and print the
  phase attribution JSON.
- Generate LUBM(1) via `scripts/bench/gen_lubm.sh` if the toolchain is available;
  otherwise fall back to a synthetic closure-heavy dataset and **say so** in the
  report (no silent substitution). Run A/B, record numbers in `docs/benchmarks.md`
  and a short attribution note; report how much of the 3× gap the swap closes
  and what remains (cross-ref the `rdf:type`-scan work, #133/#134).

### 6. Docs sync (closing commit)

- `TASKS.md`: `[v]` → `[x]` on index + body, drop the wip tag, keep `(#61)`.
- `docs/architecture.md`: flip the SPEC-05-closure-wiring / LUBM A/B Status.
- `docs/benchmarks.md`: materialize row with the A/B numbers.
- PR body: `Closes #61`.

## Execution order (atomic commits)

1. Cargo feature + optional dep (compiles, feature off = no change).
2. `GraphBlasBackend` module + unit tests (feature on).
3. Injectable `Engine` (`BackendChoice` + `with_backend`), wire `load()`.
4. Differential parity test (feature on) — the gate.
5. Profiling instrumentation in `materialize_with` + `bench-rdfox --backend`.
6. Run LUBM(1) (or documented synthetic) A/B; record in `docs/benchmarks.md` + note.
7. Docs sync + close.

## Verification

`cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`;
`cargo test --workspace`; `cargo test -p horndb-owlrl --features graphblas-backend`
(differential parity). Plus the A/B profiling run.
