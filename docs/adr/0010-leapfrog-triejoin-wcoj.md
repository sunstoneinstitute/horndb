# ADR-0010: Leapfrog Triejoin worst-case-optimal join as the join substrate

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-00 (subsystem decomposition), SPEC-03, and `docs/architecture.md`.

## Context

Triple-pattern BGPs with cyclic or many-way joins blow up intermediate results under traditional binary (pairwise) joins. Worst-case-optimal joins (WCOJ) bound output size by the AGM bound and avoid materializing large intermediates.

## Decision

Route triple-pattern matching through a worst-case-optimal join executor:

- Use a Leapfrog Triejoin WCOJ executor (`horndb-wcoj`) as the join substrate.
- Provide a cost-based binary-hash fallback, selected by a cardinality-driven planner.
- Keep the executor generic over its source (GATs, no `Box<dyn>` in the hot path).
- Support cancellation within ≤100 ms.

## Consequences

- `+` Provably optimal on skewed/cyclic workloads — measured ~34× faster than binary-hash on the canonical skewed 4-cycle.
- `+` The planner picks WCOJ vs binary by estimated cardinality.
- `−` More complex than a plain hash-join engine; a repeated-pattern over-production correctness bug had to be found and fixed, now pinned by a differential fuzzer.

## Related

- Governing specs: `docs/specs/SPEC-00-vision.md` (subsystem decomposition), `docs/specs/SPEC-03-query-engine.md`.
- Architecture: `docs/architecture.md` §5.
- Sibling ADRs: ADR-0005, ADR-0011.
