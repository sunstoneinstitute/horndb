# `horndb-incremental` (SPEC-06) — agent notes

DBSP-style Z-set deltas, change feed, checkpointing.

- Stage 1 began **insertion-only**. Retraction is landing incrementally — F6
  retraction-across-joins has merged (#45) and F7 in-flight reader visibility
  via refcounted `Circuit::snapshot()` MVCC handles has merged (#46). Backing
  snapshots onto SPEC-02 storage MVCC is still tracked under task/issue #6.
  Treat the code as the source of truth for what currently works.

See `FUTURE-WORK.md` and SPEC-06 for the retraction/MVCC roadmap.
