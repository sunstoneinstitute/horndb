# Follow-up Tasks

Outstanding work deferred from the Stage-1 pass (2026-05-24), ordered by
priority within each category. This file tracks **current state only** — closed
tasks collapse to a one-line link in [Done](#done-for-traceability).

When a task is picked up, move it to its own commit / PR and check it off here
(and in the index) in the same commit.

> **Maintenance:** the [Index](#index) is the TOC — one line per task, mirroring
> its checkbox state, **priority**, and _category_. Each open task mirrors a
> GitHub issue (`sunstoneinstitute/horndb`) via its `([#N](…))` link, labelled
> `priority: …` + `category: …` to match. Add/complete/retitle/re-prioritise a
> task and its issue together — see `CLAUDE.md` → "Keep the docs in sync".
>
> **Priority** = urgency (CRITICAL/HIGH/MEDIUM/LOW). **Category** = type of work:
> _Correctness_ · _Performance_ · _Completeness_ · _Conformance_ · _Tooling_ ·
> _Operational_ · _Maintainability_.

## Index

- [x] **HIGH** · _Completeness_ — SPEC-07 SPARQL aggregation (`GROUP BY`/`COUNT`/`SUM`) + expanded `FILTER`/`BIND`/`IF` expressions (trainmarks-blocking) ([#66](https://github.com/sunstoneinstitute/horndb/issues/66))
- [x] **HIGH** · _Completeness_ — SPEC-07 wire SPARQL frontend onto real storage + WCOJ + materialized closure (trainmarks-blocking) ([#67](https://github.com/sunstoneinstitute/horndb/issues/67))
- [x] **HIGH** · _Completeness_ — SPEC-07 pattern-based Update (`INSERT`/`DELETE … WHERE`) (trainmarks-blocking) ([#51](https://github.com/sunstoneinstitute/horndb/issues/51))
- [x] **MEDIUM** · _Completeness_ — SPEC-02 storage (HDT cold tier, CXL/NVMe tiering, MVCC, …) ([#3](https://github.com/sunstoneinstitute/horndb/issues/3))
- [v] **MEDIUM** · _Completeness_ — SPEC-04 rules (`dt-*`, `cls-maxc*`, proof recording, …) ([#4](https://github.com/sunstoneinstitute/horndb/issues/4)) — _wip: 8642e92d@Stigs-MacBook-Pro.local · task-38-proof-recording · 2026-06-17T10:27:12Z_
- [ ] **MEDIUM** · _Completeness_ — SPEC-05 closure (retraction path, GPU backend, LAGraph) ([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
- [v] **MEDIUM** · _Completeness_ — SPEC-06 incremental (retraction, MVCC) ([#6](https://github.com/sunstoneinstitute/horndb/issues/6)) — _wip: a175a6d8@Stigs-MacBook-Pro.local · task-6-f6-retraction-across-joins · 2026-06-17T10:46:50Z_
- [ ] **MEDIUM** · _Completeness_ — SPEC-07 SPARQL (property paths, full `Update`, GSP, `EXPLAIN`, …) ([#7](https://github.com/sunstoneinstitute/horndb/issues/7))
- [ ] **MEDIUM** · _Completeness_ — SPEC-08 ML (LLM→SPARQL endpoint, FAISS, audit endpoint, …) ([#8](https://github.com/sunstoneinstitute/horndb/issues/8))
- [ ] **MEDIUM** · _Completeness_ — SPEC-10 rdflib-compatible Python API (PyO3 bindings) ([#9](https://github.com/sunstoneinstitute/horndb/issues/9))
- [ ] **MEDIUM** · _Conformance_ — SPEC-01 harness (full W3C/ORE/LDBC/UOBM suites; LUBM RDFox A/B wired at N=1) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
- [ ] **MEDIUM** · _Performance_ — Closure valued-reasoning readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11))
- [ ] **MEDIUM** · _Performance_ — Valued-closure / custom-semiring acceleration ([#12](https://github.com/sunstoneinstitute/horndb/issues/12))
- [ ] **LOW** · _Operational_ — Disk pressure during multi-agent runs (rocksdb) ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
- [ ] **LOW** · _Operational_ — 1Password SSH agent reliability ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))
- [x] **LOW** · _Tooling_ — tasks.sh portability on macOS (flock / gawk match / GNU date) ([#78](https://github.com/sunstoneinstitute/horndb/issues/78))

Closed tasks are listed in [Done](#done-for-traceability).

## HIGH — SPARQL query surface (trainmarks-blocking)

These three SPEC-07 increments were promoted from MEDIUM (2026-06-08) because
they gate running the **trainmarks** RDF benchmark
(`https://datatreehouse.github.io/trainmarks/`): a six-query, three-scale
(~100K / ~1M / ~10M triple) SPARQL throughput suite with **no OWL reasoning**.
They stay listed as increments of the SPEC-07 epic ([#7](https://github.com/sunstoneinstitute/horndb/issues/7)).

- [x] **SPARQL aggregation + expanded expressions.**
  ([#66](https://github.com/sunstoneinstitute/horndb/issues/66))
  `GROUP BY` / `COUNT` / `SUM` / `DISTINCT`-count and the `FILTER`/`BIND`
  expression surface (`<=` / `>=` / `IN` / `NOT IN` / arithmetic / `IF` /
  functions beyond `= < > && || ! BOUND`) return `UnsupportedAlgebra` in
  `crates/sparql/src/algebra/translate.rs`. Blocks four of the six trainmarks
  queries (and the LDBC SPB aggregation mix).

- [x] **Wire the SPARQL frontend onto real storage + WCOJ + closure.**
  ([#67](https://github.com/sunstoneinstitute/horndb/issues/67))
  The runtime executes against the standalone in-memory
  `crates/sparql/src/exec/mem.rs::MemStore` (naive nested-loop `scan_bgp`, no
  `horndb-wcoj` / `horndb-storage` / `horndb-owlrl` dependency). It times out
  at ~500K triples, so it cannot reach the 10M-triple scale; also fixes
  decoupled data (served store repopulated from a flat dump) and literal-as-IRI
  term coercion (wrong `ORDER BY` / literal comparisons).

- [x] **Pattern-based Update (`INSERT`/`DELETE … WHERE`).**
  ([#51](https://github.com/sunstoneinstitute/horndb/issues/51))
  Only `INSERT DATA` / `DELETE DATA` ship today; trainmarks includes a
  conditional `DELETE`/`INSERT … WHERE` update (the `BIND`/`IF` expression half
  is #66).

## MEDIUM — Stage-2 scope (deferred per plans)

Each line is an epic; delivered increments are noted, remaining increments are
the open work. Pull from this list when the corresponding Stage-1 slice settles.

- [x] **SPEC-02 storage.** ([#3](https://github.com/sunstoneinstitute/horndb/issues/3))
  Stage-1 increments all delivered: compressed columnar source (#15),
  six index orderings (#16),
  HDT-derived snapshot export/import ([#17](https://github.com/sunstoneinstitute/horndb/issues/17)),
  Turtle / N-Quads import ([#18](https://github.com/sunstoneinstitute/horndb/issues/18)),
  copy-on-write snapshot isolation ([#19](https://github.com/sunstoneinstitute/horndb/issues/19)).
  Deferred to Stage 2/3 (open a new task when pulled in): CXL/NVMe placement
  (SPEC-09), persistent dictionary (Marisa/FST), true per-tuple MVCC.

- [v] **SPEC-04 rules.** ([#4](https://github.com/sunstoneinstitute/horndb/issues/4))
  Remaining: production proof recording (F4) + `proof(t)` API ([#38](https://github.com/sunstoneinstitute/horndb/issues/38)),
  `rdf:type` skew parallelism (F5) ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)),
  literal-value rules `dt-eq`/`dt-diff`/`dt-not-type` ([#40](https://github.com/sunstoneinstitute/horndb/issues/40)).
  Delivered: `dt-type1`/`dt-type2` subsumption + `scm-eqc-rev` (#34),
  unqualified max-cardinality `cls-maxc1`/`cls-maxc2` (#35),
  qualified max-cardinality `cls-maxqc1`-`cls-maxqc4` (#36),
  `prp-adp` all-disjoint-properties (#37).
  Deferred: datatype value-space *intersection* (`I5.8-008/009-pe`),
  user-defined Datalog frontend (Stage-2).

- [ ] **SPEC-05 closure.** ([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
  Remaining: deletion/retraction half of F6 (blocked on SPEC-06 #6).
  Delivered: incremental insertion-path transitive closure (#42).
  Deferred: GPU GraphBLAS backend (SPEC-09), LAGraph adoption, perf tuning
  (`GrB_Matrix_dup` clone, `(min,+)` semiring, nnz-threshold routing).

- [v] **SPEC-06 incremental.** ([#6](https://github.com/sunstoneinstitute/horndb/issues/6))
  Remaining: F6 correct retraction across joins ([#45](https://github.com/sunstoneinstitute/horndb/issues/45)),
  F7 in-flight reader visibility / MVCC snapshots ([#46](https://github.com/sunstoneinstitute/horndb/issues/46)).
  Delivered: F5 closure-operator deltas (#44).
  Deferred: distributed timely-dataflow (SPEC-09).

- [ ] **SPEC-07 SPARQL.** ([#7](https://github.com/sunstoneinstitute/horndb/issues/7))
  Remaining: non-recursive property paths `|`/`!`/`?` ([#49](https://github.com/sunstoneinstitute/horndb/issues/49)),
  Kleene paths `*`/`+` via closure ([#50](https://github.com/sunstoneinstitute/horndb/issues/50)),
  graph-management Update `LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY` ([#52](https://github.com/sunstoneinstitute/horndb/issues/52)),
  `EXPLAIN` pragma ([#53](https://github.com/sunstoneinstitute/horndb/issues/53)),
  Graph Store Protocol ([#54](https://github.com/sunstoneinstitute/horndb/issues/54)),
  backward-chained entailment mode + per-query pragma ([#55](https://github.com/sunstoneinstitute/horndb/issues/55)),
  streaming result serialization ([#56](https://github.com/sunstoneinstitute/horndb/issues/56)),
  SPARQL XML results + Turtle CONSTRUCT/DESCRIBE ([#57](https://github.com/sunstoneinstitute/horndb/issues/57)).
  Promoted HIGH (trainmarks): #51, #66, #67 — see section above.
  Delivered: `DESCRIBE` one-level CBD (#48).
  Deferred: `SERVICE` federation, RDF 1.2 SPARQL surface, GeoSPARQL.

- [ ] **SPEC-08 ML.** ([#8](https://github.com/sunstoneinstitute/horndb/issues/8))
  F3 LLM → SPARQL HTTP endpoint, real FAISS-backed `CandidateGenerator`, HTTP
  audit endpoint + cost reporting, training-data leakage controls. (Stage-1
  ships the traits + in-process scaffolding only.)

- [ ] **SPEC-10 rdflib-compatible Python API.** ([#9](https://github.com/sunstoneinstitute/horndb/issues/9))
  Build the PyO3/maturin binding layer per
  `docs/specs/SPEC-10-rdflib-compatible-python-api.md`: rdflib-shaped terms,
  `Graph`/`Dataset` facades, core `add`/`remove`/`triples`/`query`/`update`,
  Turtle + N-Triples parse/serialize, SPARQL passthrough to SPEC-07, plus a
  `rdflib-compat` harness subset differential-tested against upstream `rdflib`.
  No crate exists yet; SPEC-10 has no Stage-1 plan. Open decision: import-path
  strategy (shim vs. literal `rdflib` name).

- [ ] **SPEC-01 harness.** ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
  Replace the hand-picked 50-case OWL 2 RL subset with the full W3C OWL 2 +
  SPARQL 1.1 suites, full ORE 2015 corpus, LDBC SPB SF3/SF5 runs, LUBM + UOBM
  coverage, and broader RDFox A/B (publication gated on license review).
  LUBM materialization RDFox A/B is wired at N=1 (`compare-rdfox.sh --lubm`,
  exact closure-count parity); LUBM-100 and the wider corpora are outstanding.

- [ ] **Closure valued-reasoning readiness metrics.** ([#11](https://github.com/sunstoneinstitute/horndb/issues/11))
  Instrument the closure path *before* building any custom-semiring work, so the
  call is measured not guessed. Per run (harness + a `BENCHMARKS.md` row): matrix
  dimension `N` / `nnz` / density; iterations-to-fixpoint and work/iteration;
  `GrB_mxm` time for the valued semiring vs. a boolean baseline on the same shape;
  user-defined-op vs. built-in FactoryKernel throughput (the JIT/PreJIT multiplier);
  carrier shape (scalar=Fork A / structured=Fork B); valued-query frequency + SLO.
  Decision rule to record: stay on built-in semirings while the carrier is scalar
  or `N` is small; custom semiring only for a structured carrier; PreJIT only when
  the measured generic-kernel share × speedup crosses the SLO.

- [ ] **Valued-closure / custom-semiring acceleration.** ([#12](https://github.com/sunstoneinstitute/horndb/issues/12))
  Depends on #11. For Sunstone annotated reasoning (GTIO weighted edges + SKOS
  crosswalk confidences via RDF 1.2 triple terms). Ladder in cost order:
  (1) **Fork A** — scalar confidence on built-in `max-times` / `min-plus`
  semirings, no JIT; deliver a bench against the GTIO/SKOS crosswalk graph.
  (2) **Fork B** — structured carrier `(confidence, SKOS match-type, provenance)`
  via a user type + user semiring on GraphBLAS's generic kernel.
  (3) **PreJIT** — bake specialized kernels into the vendored `libgraphblas`
  *only if* the metrics show the generic kernel hurts at scale.
  Done-when: Fork A bench green on the live crosswalk graph, #11 metrics
  populated, and a measured decision on Fork B/PreJIT — then open the SPEC-05
  addendum. Cross-refs: SPEC-05, SPEC-02 (RDF 1.2), SPEC-06, rdf-registry #9/#10/#11.

## LOW — Operational

- [ ] **Disk pressure during multi-agent runs.** ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
  `oxrocksdb-sys` (pulled in transitively by the harness via `oxigraph`)
  compiles a ~700 MB artifact per worktree, which exhausted free space on `/`
  during the 2026-05-24 parallel pass (surfaced as misleading "1Password failed
  to fill whole buffer" signing errors). The vendored GraphBLAS is already
  de-duplicated across worktrees; rocksdb is the remaining driver — point
  `CARGO_TARGET_DIR` at a shared path, prune the rocksdb dep, or document a
  ≥15 GB-free precondition. Stays open until rocksdb duplication is addressed.

- [ ] **1Password SSH agent reliability.** ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))
  The agent intermittently returns "no identities" / "communication with agent
  failed" during long agent sessions even when the desktop app is unlocked. Fix:
  keep the app foregrounded during long sessions, or pre-cache an unencrypted
  signing key for CI. (Bypassing signing is not acceptable — global rule.)

- [x] **tasks.sh portability on macOS.** ([#78](https://github.com/sunstoneinstitute/horndb/issues/78))
  `.claude/scripts/tasks.sh` needs `flock(1)` (absent on Darwin — installed
  locally via Homebrew), and its `claims`/`reap` subcommands silently fail on
  BSD awk (gawk-only 3-arg `match()`) and BSD `date` (no `-d`). Make the lock
  portable or probe with a clear error, and rewrite `parse_claims`/age
  computation portably so orphan detection works on macOS.

## Done (for traceability)

Completed tasks; issues closed, links kept.

- [x] **CRITICAL** · _Correctness_ — SPEC-03 WCOJ over-produced on BGPs with repeated patterns (leapfrog prime-time iter sort).
- [x] **HIGH** · _Correctness_ — OWL 2 RL closure "over-derivation" vs reference on LUBM(1) ([#59](https://github.com/sunstoneinstitute/horndb/issues/59)) — was a harness-completeness gap; parity now exact (delta 0).
- [x] **HIGH** · _Maintainability_ — Workspace-wide `cargo clippy -- -D warnings` green; harness exclusion dropped from pre-push.
- [x] **HIGH** · _Performance_ — SPEC-03 WCOJ 4-cycle meets ≥10× gate ([#1](https://github.com/sunstoneinstitute/horndb/issues/1)) — ~34× on the canonical skewed win case (`SyntheticGraph::skewed_four_cycle`).
- [x] **HIGH** · _Performance_ — GraphBLAS closure backend wired + injectable into the owlrl Engine ([#61](https://github.com/sunstoneinstitute/horndb/issues/61)); profiling shows the LUBM timing gate is `rdf:type`-scan-bound (#2/#39), not closure-bound.
- [x] **HIGH** · _Completeness_ — Workspace migrated to oxrdf 0.3 + end-to-end RDF 1.2 triple-term support (`<<( s p o )>>`, gated by `SparqlConfig::rdf12`).
- [x] **HIGH** · _Conformance_ — W3C RDF 1.2 N-Triples syntax subset (`rdf12-n-triples`, 4 positive + 6 negative) in `harness/selected.toml`.
- [x] **MEDIUM** · _Performance_ — SPEC-04 eq-rep-p skew ([#2](https://github.com/sunstoneinstitute/horndb/issues/2)) — class-canonical union-find pass (`eq_rep_p_opt.rs`), default `Optimized`; downstream `rdf:type` partition-by-class-id (F5) remains under #39.
- [x] **MEDIUM** · _Conformance_ — W3C OWL 2 RL test-suite ingestion pipeline (`harness extract-owl2-rl`; 91 cases → 78 green in `[suites.owl2-w3c-rl]`, reds in `KNOWN-MANIFEST-BUGS.md`).
- [x] **LOW** · _Tooling_ — Vendored SuiteSparse:GraphBLAS as a git submodule (`v10.3.0`, static, OpenMP, checked-in bindings); supersedes the runner-install task.
- [x] **LOW** · _Maintainability_ — Consolidated `selected.toml` into the single root file (`[sparql_query]` table).
- [x] **LOW** · _Maintainability_ — Plans/specs cross-reference cleanup (`docs/specs/README.md` Plan column).
- [x] **LOW** · _Tooling_ — CI installs GraphBLAS on runners (superseded by the vendored submodule above).
- [x] **LOW** · _Completeness_ — Wired `horndb_owlrl::Engine` to satisfy the harness `Reasoner` trait.

### Archive — project bootstrap

- [x] 9 specs written (SPEC-00..09); 9 plans (one per spec; SPEC-09 roadmap-only).
- [x] 7 implementation subagents dispatched in parallel under worktree isolation; all landed signed commits into main.
