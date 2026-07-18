# `horndb-incremental` (SPEC-06) — agent notes

DBSP-style Z-set deltas, change feed, checkpointing.

- Stage 1 began **insertion-only**. Retraction is landing incrementally — F6
  retraction-across-joins has merged (#45), F7 in-flight reader visibility
  via refcounted `Circuit::snapshot()` MVCC handles has merged (#46), and
  **closure-path retraction** has merged (#5): a `ClosureInferred` row whose
  base support is retracted is withdrawn via `ClosureRule::apply_retract_delta`
  + SPEC-05's `delete_transitive_edges`. Backing snapshots onto SPEC-02 storage
  MVCC is now tracked under SPEC-24 S6 (#215, Stage-2 epic #186). Treat the
  code as the source of truth for what currently works.

See `FUTURE-WORK.md` and SPEC-06 for the retraction/MVCC roadmap. The
Stage-2 contract is `docs/specs/SPEC-24-incremental-stage2.md` (epic #186,
phase sub-issues #210–#217): delta-incremental retraction, change-feed
net-delta + backpressure, engine wiring, WAL, MVCC backing, join runtime.
