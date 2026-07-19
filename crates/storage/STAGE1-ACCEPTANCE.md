# SPEC-02 Stage-1 Acceptance Record

Run on: <fill in: hostname, kernel, CPU, DRAM channels & speed>
Commit: <fill in: git rev-parse HEAD>
Date: <fill in>

## SPEC-02 acceptance criteria addressed in Stage 1

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | LUBM-100 import ≤30 s | <PASS/FAIL> | `cargo bench -p horndb-storage --bench load_lubm` with `LUBM_NT=...lubm100.nt`; elapsed = <ms> |
| 2 | LUBM-8000 import ≤30 min | DEFERRED | `SPEC-25` S6 ([#230](https://github.com/sunstoneinstitute/horndb/issues/230)) — bench harness exists, just not run yet |
| 3 | LUBM-8000 footprint ≤55 GB | DEFERRED | `SPEC-25` S6 ([#230](https://github.com/sunstoneinstitute/horndb/issues/230)) |
| 4 | Sequential scan ≥80% of STREAM Triad | DEFERRED | `SPEC-25` S6 ([#230](https://github.com/sunstoneinstitute/horndb/issues/230)) — a 2026-07-07 hornbench `partition_scan` hit ~104% Triad (SPEC-12), but not on the LUBM-8000 corpus the criterion names |
| 5 | HDT round-trip isomorphism | IMPLEMENTED | F9 delivered: `snapshot/` exports the default graph to an HDT-*derived* compact format and re-imports it. Blank-node labels are preserved, so round-trip yields exact triple-set equality (a strictly stronger property than isomorphism under blank-node renaming). Covered by `crates/storage/tests/snapshot_roundtrip.rs`. Named-graph snapshots remain a follow-up — export errors rather than silently dropping non-default-graph data. |
| 6 | All-six orderings for top-10 predicates | IMPLEMENTED | F4 delivered ([#16](https://github.com/sunstoneinstitute/horndb/issues/16)): `ordering.rs` + `partition.rs` materialise the object-major layout (eager for hot predicates, lazy for cold); `Store::top_predicates` + `scan_predicate_ordered` query any of the six orderings. Covered by `crates/storage/tests/six_orderings.rs`. |

## Stage-1 surfaced figures

- LUBM-100 triple count: <fill in>
- LUBM-100 load elapsed: <fill in> ms (≈ <Mtriples/s>)
- LUBM-100 dictionary size: <fill in>
- LUBM-100 footprint via `Store::report_footprint()`: <fill in> bytes (<fill in> B/triple)
- W3C harness selected subset run (`cargo test -p horndb-harness`): <PASS/FAIL>

## Out-of-scope items tracked as Future Work

Stage-2 items below are now specified in `docs/specs/SPEC-25-storage-stage2.md`
(epic [#187](https://github.com/sunstoneinstitute/horndb/issues/187)); phase
sub-issues in parentheses.

- CXL/NVMe cold-tier placement (SPEC-02 NF4, SPEC-09 — Stage 3; the cold-tier *seam* is `SPEC-25` S5 [#229](https://github.com/sunstoneinstitute/horndb/issues/229))
- True per-tuple-visibility MVCC + delete path (`SPEC-25` S1 [#225](https://github.com/sunstoneinstitute/horndb/issues/225), intersects SPEC-06/SPEC-24 S6). The Stage-1 substitute, copy-on-write snapshot isolation (SPEC-02 #19), is **delivered**: `Store::snapshot()` / `StoreSnapshot` pin a stable read transaction over an immutable versioned `TierSnapshot` (see `INTEGRATION-NOTES.md`).
- Named-graph / quad snapshots (`SPEC-25` S4 [#228](https://github.com/sunstoneinstitute/horndb/issues/228) — Stage-1 snapshot export covers the default graph only; export errors on named-graph data)
- rdfhdt wire-format compatibility (cross-tool interop — explicit non-goal of the Stage-1 snapshot, and still a `SPEC-25` non-goal)
- Persistent on-disk dictionary (`SPEC-25` S2 [#226](https://github.com/sunstoneinstitute/horndb/issues/226))
- HDT input format as a bulk-import path (SPEC-02 F8 — Turtle and N-Quads now ship via `loader::{turtle, nquads}`, [#18](https://github.com/sunstoneinstitute/horndb/issues/18); HDT ingest remains a follow-up, `SPEC-25` non-goal until a consumer needs it)
- Crash-consistent checkpointing + WAL (`SPEC-25` S3 [#227](https://github.com/sunstoneinstitute/horndb/issues/227) — Stage 1 is in-memory only)
