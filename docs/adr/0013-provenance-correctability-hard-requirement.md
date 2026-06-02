# ADR-0013: Provenance / correctability as a hard requirement

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md`, `docs/specs/SPEC-04` (rules), `docs/specs/SPEC-07` (SPARQL), and `docs/architecture.md`.

## Context

Inferred triples in production graphs — PROV-O-backed graphs are a primary Sunstone workload — must be explainable and correctable. This requirement is precisely what rules out replacing symbolic reasoning with embeddings: a probabilistic model cannot produce an auditable derivation trail. "Provenance preserved" is one of three non-negotiable project success criteria.

## Decision

Every inferred triple must be traceable back to its premises, supporting on-demand re-derivation.

- Each inference records a proof tree of `(rule_id, premise_ids[])`.
- Provenance is folded into SPEC-04 (rules) and SPEC-07 (SPARQL) rather than carved out as a separate spec.
- The proof structure supports on-demand re-derivation of any inferred triple.

## Consequences

- `+` Auditable inference; underpins correction and (future) retraction reasoning.
- `−` Stage 1 ships only a stub `Provenance`; production proof recording (SPEC-04 F4) and proof retrieval (NF4) are planned, not built.
- `−` Proof storage adds overhead once the real recorder lands.

## Related

- `docs/specs/SPEC-00-vision.md` (bet 6, success criteria)
- `docs/specs/SPEC-04`
- `docs/specs/SPEC-07`
- `docs/architecture.md` §1, §13
- Siblings: ADR-0012, ADR-0008
