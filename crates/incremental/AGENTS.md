# `horndb-incremental` (SPEC-06) — agent notes

DBSP-style Z-set deltas, change feed, checkpointing.

- **Rule retraction is delta-incremental** (SPEC-24 S1, #210, `PLAN-24-01`):
  `Circuit::tick()` runs one unified incremental fixpoint; retraction ticks
  run a two-phase overdelete / re-derive pass driven by per-row per-rule
  weight traces (`rule_weights`). The Stage-1 full recompute survives only as
  a config-gated fallback (`Circuit::new_with_recompute_fallback()`) and as
  the differential-test oracle. Earlier increments: F6 recompute-and-diff
  (#45), F7 in-flight reader visibility via refcounted `Circuit::snapshot()`
  MVCC handles (#46), closure-path retraction (#5). Backing snapshots onto
  SPEC-02 storage MVCC is tracked under SPEC-24 S6 (#215, Stage-2 epic #186).
  Treat the code as the source of truth for what currently works.

See `FUTURE-WORK.md` and SPEC-06 for the retraction/MVCC roadmap. The
Stage-2 contract is `docs/specs/SPEC-24-incremental-stage2.md` (epic #186;
S1 delivered, remaining phase sub-issues #211–#217): delta-incremental
closure retraction, change-feed net-delta + backpressure, engine wiring,
WAL, MVCC backing, join runtime.
