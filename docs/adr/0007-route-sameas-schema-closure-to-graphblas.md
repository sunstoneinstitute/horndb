# ADR-0007: Route owl:sameAs and schema closure to the GraphBLAS EQREL backend

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-04 (F6), SPEC-05, and `docs/architecture.md`.

## Context

`owl:sameAs` equivalence and `rdfs:subClassOf` / `rdfs:subPropertyOf` closure are closure problems, not general rule firing. Deriving `eq-sym` and `eq-trans` inside the rule engine duplicates work the GraphBLAS backend does better, and maintaining two derivation paths risks divergence between them.

## Decision

Route equivalence and schema closure to the closure backend rather than re-deriving them in the rule engine.

- SPEC-04 routes `owl:sameAs` to SPEC-05's EQREL / union-find.
- SPEC-04 routes `rdfs:subClassOf` / `rdfs:subPropertyOf` closure to the GraphBLAS backend.
- The rule engine does not re-derive `eq-sym` / `eq-trans` (F6).
- The `eq-rep-p` predicate-position rewrite uses a semantics-preserving class-canonical union-find path (default `EqRepPStrategy::Optimized`), proven equivalent to the naïve oracle by a differential proptest.

## Consequences

+ Single source of truth for equivalence and closure; less redundant derivation.
+ Measurable speedups on `sameAs`-heavy graphs.
− Cross-crate coupling: `owlrl` depends on the closure-routing boundary, which must stay correct as rules evolve.

## Related

- Governing specs: `docs/specs/SPEC-04-owlrl-rules.md` (F6), `docs/specs/SPEC-05-closure-backend.md`.
- Architecture: `docs/architecture.md` §6, §7.
- Siblings: ADR-0006.
