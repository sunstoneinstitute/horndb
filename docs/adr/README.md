# Architecture Decision Records

This is HornDB's ADR log: the standing record of *why* the architecture is the
way it is. Each ADR captures one decision — its context, the decision itself,
and the consequences — in the lightweight [Nygard
format](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions).

These records were **extracted retrospectively** (2026-06-02) from
`../specs/SPEC-00..10-*.md`, the dated design specs, and `../architecture.md`.
They document decisions already made and (mostly) implemented at Stage 1, not
new proposals. Where a decision is only partially realised, the **Consequences**
section says so.

## How the docs relate

- **SPECs** (`../specs/`) — the subsystem *contracts*: what each part must do.
- **ADRs** (here) — the cross-cutting *decisions* and rationale behind those contracts.
- **`../architecture.md`** — the *current-state* map, with a Status field per subsystem.
- **`../../TASKS.md`** — the *outstanding work* to close the gaps.

When the architecture changes, add a new ADR rather than rewriting an old one;
mark the superseded record's **Status** as `Superseded by ADR-NNNN`.

## Index

| ADR | Decision | Governing SPEC(s) |
|-----|----------|-------------------|
| [0001](0001-scope-owl2-rl-not-dl.md) | Scope to OWL 2 RL, not OWL 2 DL; RDF + SPARQL only | SPEC-00 |
| [0002](0002-harness-first-conformance-gated.md) | Harness-first, conformance-gated development | SPEC-00, SPEC-01 |
| [0003](0003-rust-layered-workspace.md) | Rust on a layered nine-crate workspace | SPEC-00 |
| [0004](0004-compile-owl2rl-rules-ahead-of-time.md) | Compile OWL 2 RL rules ahead of time (Soufflé-style) | SPEC-04 |
| [0005](0005-hybrid-forward-backward-chaining.md) | Hybrid forward/backward-chaining over pure materialization | SPEC-00, SPEC-03, SPEC-07 |
| [0006](0006-graphblas-closure-backend.md) | SuiteSparse:GraphBLAS for the closure subset | SPEC-05 |
| [0007](0007-route-sameas-schema-closure-to-graphblas.md) | Route `owl:sameAs` + schema closure to the GraphBLAS EQREL backend | SPEC-04, SPEC-05 |
| [0008](0008-dbsp-zset-incremental-maintenance.md) | DBSP-style incremental maintenance with Z-sets (insertion-only at Stage 1) | SPEC-06 |
| [0009](0009-unified-memory-tiered-storage.md) | Unified-memory hardware as a first-class target (tiered columnar storage) | SPEC-02 |
| [0010](0010-leapfrog-triejoin-wcoj.md) | Leapfrog Triejoin worst-case-optimal join as the join substrate | SPEC-03 |
| [0011](0011-sparql11-frontend-oxrdf-stack.md) | SPARQL 1.1 frontend on the oxrdf / spargebra stack | SPEC-07 |
| [0012](0012-ml-is-advisor-not-source-of-truth.md) | Symbolic reasoner is the source of truth; ML is an opt-in advisor | SPEC-08 |
| [0013](0013-provenance-correctability-hard-requirement.md) | Provenance / correctability as a hard requirement | SPEC-00, SPEC-04, SPEC-07 |
| [0014](0014-track-rdf12-not-rdf-star.md) | Track W3C RDF 1.2 (not RDF-star), gated behind config | SPEC-00, SPEC-07 |
| [0015](0015-vendor-graphblas-static-submodule.md) | Vendor SuiteSparse:GraphBLAS as a static git submodule | SPEC-05 |

## Adding a new ADR

1. Copy the structure of an existing record (Status / Date / Source / Context / Decision / Consequences / Related).
2. Number it sequentially (`NNNN-kebab-title.md`).
3. Add a row to the index above.
4. If it changes outstanding work or current state, update `../../TASKS.md` and `../architecture.md` in the same commit (the docs-sync rule in the root `CLAUDE.md`).
