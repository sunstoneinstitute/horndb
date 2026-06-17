# `horndb-incremental` (SPEC-06) — agent notes

DBSP-style Z-set deltas, change feed, checkpointing.

- Stage 1 began **insertion-only**. Retraction is landing incrementally — F6
  retraction-across-joins has merged (#45). Full retraction + MVCC are tracked in
  task/issue #6. Treat the code as the source of truth for what currently works.

See `FUTURE-WORK.md` and SPEC-06 for the retraction/MVCC roadmap.
