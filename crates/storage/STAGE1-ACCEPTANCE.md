# SPEC-02 Stage-1 Acceptance Record

Run on: <fill in: hostname, kernel, CPU, DRAM channels & speed>
Commit: <fill in: git rev-parse HEAD>
Date: <fill in>

## SPEC-02 acceptance criteria addressed in Stage 1

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | LUBM-100 import ≤30 s | <PASS/FAIL> | `cargo bench -p reasoner-storage --bench load_lubm` with `LUBM_NT=...lubm100.nt`; elapsed = <ms> |
| 2 | LUBM-8000 import ≤30 min | DEFERRED | Stage 2 — bench harness exists, just not run yet |
| 3 | LUBM-8000 footprint ≤55 GB | DEFERRED | Stage 2 |
| 4 | Sequential scan ≥80% of STREAM Triad | DEFERRED | Stage 2 (needs the hot tier in a NUMA-pinned bench) |
| 5 | HDT round-trip isomorphism | DEFERRED | Stage 2 (no HDT support in Stage 1) |
| 6 | All-six orderings for top-10 predicates | DEFERRED | Stage 2 |

## Stage-1 surfaced figures

- LUBM-100 triple count: <fill in>
- LUBM-100 load elapsed: <fill in> ms (≈ <Mtriples/s>)
- LUBM-100 dictionary size: <fill in>
- LUBM-100 footprint via `Store::report_footprint()`: <fill in> bytes (<fill in> B/triple)
- W3C harness selected subset run (`cargo test -p reasoner-harness`): <PASS/FAIL>

## Out-of-scope items tracked as Future Work

- HDT cold-tier (SPEC-02 F9)
- CXL/NVMe tiering (SPEC-02 NF4)
- MVCC, copy-on-write snapshots (SPEC-02 risks/open questions)
- All-six index orderings (SPEC-02 F4)
- Snapshot HDT export (SPEC-02 F9)
- Persistent on-disk dictionary (SPEC-02 risks/open questions)
- Turtle, N-Quads, HDT input formats (SPEC-02 F8 — only N-Triples in Stage 1)
- Crash-consistent checkpointing (SPEC-02 NF5 — Stage 1 is in-memory only)
