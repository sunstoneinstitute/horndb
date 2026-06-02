# ADR-0008: DBSP-style incremental maintenance with Z-sets

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md` (bet 3), `docs/specs/SPEC-06-incremental-maintenance.md`, and `docs/architecture.md`.

## Context

The materialized closure must be maintained under updates rather than recomputed from scratch. DRed and forward-backward-forward counting compose poorly with point queries. DBSP Z-set differences offer an incremental model that is production-proven at Materialize Inc.

## Decision

Maintain the materialized closure with DBSP / Z-set semantics — `(triple, ±1)` multiplicities — in `horndb-incremental`:

- Linear and bilinear rule operators over Z-sets.
- A change feed exposing the delta stream.
- Checkpoint merge that collapses ±1 pairs.
- Closure-operator deltas that fold an insertion delta into the retained per-predicate closure instead of recomputing it.
- Insertion-only at Stage 1; retraction semantics (F6) are deferred.

## Consequences

+ Clean composition with point queries, on a production-proven incremental model.
+ Incremental closure deltas avoid full recompute (validated against full recompute by a differential proptest).
− Retraction and MVCC for in-flight reads are deferred to Stage 2 — this is the highest-risk spec.
− A delete cannot yet propagate through the closure.

## Related

- Governing spec: `docs/specs/SPEC-06-incremental-maintenance.md`; vision bet 3 in `docs/specs/SPEC-00-vision.md`.
- Current state: `docs/architecture.md` §8.
- Siblings: ADR-0006, ADR-0013.
