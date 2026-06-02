# ADR-0005: Hybrid forward/backward-chaining over pure materialization

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-00 (bet 1), SPEC-03 (F4/F5), SPEC-07, and `docs/architecture.md`.

## Context

The primary Sunstone workload is roughly 60% backward-chaining / 40% forward-chaining. RDFox's pure-materialization model concedes 100–1000× on backward chaining (their own published statement). Materializing everything is wasteful for a mixed workload of this shape.

## Decision

Combine forward materialization with backward chaining instead of materializing the full closure:

- Materialize the schema/transitive-closure subset: `rdfs:subClassOf`, `rdfs:subPropertyOf`, `owl:sameAs`, and transitive properties.
- Backward-chain the remainder using magic sets / SLG tabling.
- Expose both a materialized and a backward-chained entailment mode through the SPARQL frontend (SPEC-07).

## Consequences

- `+` Matches the real workload; avoids over-materialization.
- `−` Stage 1 ships only the forward/materialized half. Magic-sets/demand transformation and SLG tabling (SPEC-03 F4/F5) and the SPARQL backward-chained mode (SPEC-07 F4) are DEFERRED, so the "hybrid" is aspirational until those land.
- `−` This is the largest gap between the project's framing bet and current code.

## Related

- Governing specs: `docs/specs/SPEC-00-vision.md` (bet 1), `docs/specs/SPEC-03-query-engine.md` (F4/F5), `docs/specs/SPEC-07-*`.
- Architecture: `docs/architecture.md` §1 (bet table), §5, §9.
- Sibling ADRs: ADR-0010, ADR-0001.
