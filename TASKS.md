# Follow-up Tasks

Outstanding work deferred from the Stage-1 pass (2026-05-24), ordered by
priority within each category. This file tracks **current state only** — closed
tasks collapse to a one-line link in [Done](#done-for-traceability).

When a task is picked up, move it to its own commit / PR and check it off here
(and in the index) in the same commit.

> **Maintenance:** the [Index](#index) is the TOC — one line per task, mirroring
> its checkbox state, **priority**, and _category_. Each open task mirrors one
> GitHub issue (`sunstoneinstitute/horndb`) via its `([#N](…))` link, on both the
> index line and the body heading, labelled with one `priority:` label
> (`critical`/`high`/`medium`/`low`) and one `category:` label to match. Keep task
> and issue in lockstep, in the same change:
>
> - **Add a task** → open an issue with the matching `priority:` + `category:`
>   labels, then put its `([#N](url))` link on both the index line and the body
>   heading. `gh issue create --title … --label "priority: …" --label "category: …" --body-file …`.
> - **Complete a task** (`[ ]` → `[x]`) → `gh issue close N`. Keep the link for traceability.
> - **Retitle / re-prioritise / re-categorise** → `gh issue edit N` to update the
>   title and swap the `priority:`/`category:` labels so they still match.
> - **Remove a task** → `gh issue close N` (comment why) and drop its `TASKS.md` lines.
>
> The `priority:`/`category:` labels are the GitHub mirror of the taxonomy below; if
> you add a new one here, `gh label create` it first. When a task changes, also
> update `docs/architecture.md` — see `CLAUDE.md` → "Keep the docs in sync".
>
> **Priority** = urgency (CRITICAL/HIGH/MEDIUM/LOW). **Category** = type of work:
> _Correctness_ · _Performance_ · _Completeness_ · _Conformance_ · _Tooling_ ·
> _Operational_ · _Maintainability_.

## Index

- [ ] **HIGH** · _Performance_ — SPARQL aggregation runtime: id-based bindings + hash group-by + streaming (12× SPB gap) ([#128](https://github.com/sunstoneinstitute/horndb/issues/128))
- [ ] **MEDIUM** · _Performance_ — LDBC SPB nightly: scale to true SF=0.256 (256M triples) + editorial agents ([#125](https://github.com/sunstoneinstitute/horndb/issues/125))
- [ ] **LOW** · _Operational_ — Disk pressure during multi-agent runs (rocksdb) ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
- [ ] **LOW** · _Operational_ — 1Password SSH agent reliability ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))

Closed tasks are listed in [Done](#done-for-traceability).

## HIGH — Performance

- [ ] **SPARQL aggregation runtime: id-based bindings + hash group-by + streaming.**
  ([#128](https://github.com/sunstoneinstitute/horndb/issues/128))
  The SPB nightly serves ~12 aggregation-qps where GraphDB Free serves ~150 (~12×).
  Diagnosis (in-process against the real `HornBackend`; harness at
  `crates/sparql/examples/agg_profile.rs`) shows the gap is structural in the SPARQL
  runtime, not codegen — **PGO is the wrong lever** here. Three causes, by impact:
  (1) the dictionary is defeated at the SPARQL boundary — `HornBackend::scan_bgp`
  (`exec/horn.rs:597-606`) decodes every `TermId` back to a heap `Term::Iri(String)`
  and `Bindings` is `BTreeMap<String, Term>` (`exec/mod.rs:19-22`), so `COUNT(*)` spends
  269 ms allocating 400k×3 strings to count what `len()` knows instantly; (2) `DISTINCT`
  dedup is a linear scan (`runtime.rs::dedup_terms`, O(n·d)→O(n²)) — **fixed separately**
  by the HashSet PR; (3) no streaming / no projection or aggregate pushdown
  (`runtime.rs:26-28` materializes a full `Vec<Bindings>` per node; `plan/planner.rs`
  is a 1:1 lowering with no cost model). Scope: hash group-by, id-based `Bindings`
  (decode to strings only at serialization), then streaming + pushdown. Revisit PGO
  only after this lands. See `docs/architecture.md` §9 and `BENCHMARKS.md`.

## MEDIUM — Performance

- [ ] **LDBC SPB nightly: scale to true SF=0.256 + editorial agents.**
  ([#125](https://github.com/sunstoneinstitute/horndb/issues/125))
  The nightly SPB job (`.github/workflows/nightly.yml`) runs end-to-end on
  `hornbench` but only at **feasible scale** — a ~512k-triple materialized
  closure, aggregation-only, `editorialAgents=0`, so the headline metric is
  `aggregation-qps`, not the LDBC `editorial-qps`. Scale to the true SF=0.256
  (256M-triple) dataset and enable editorial (CW insert/update/delete) agents:
  materialize the 256M closure on `hornbench` and confirm both engines (HornDB
  `serve`, GraphDB Free `spb` repo) can hold it; flip `editorialAgents` on in
  `crates/harness/scenarios/spb-nightly.properties` and reconcile the nominal
  `datasetSize` (currently 18,644,617) with what is actually loaded; move the
  trend metric to `editorial-qps`. See `BENCHMARKS.md` and the SPB nightly row
  in `docs/architecture.md`.

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

## Done (for traceability)

Completed tasks; issues closed, links kept.

- [x] **CRITICAL** · _Correctness_ — SPEC-03 WCOJ over-produced on BGPs with repeated patterns (leapfrog prime-time iter sort).
- [x] **HIGH** · _Correctness_ — OWL 2 RL closure "over-derivation" vs reference on LUBM(1) ([#59](https://github.com/sunstoneinstitute/horndb/issues/59)) — was a harness-completeness gap; parity now exact (delta 0).
- [x] **HIGH** · _Maintainability_ — Workspace-wide `cargo clippy -- -D warnings` green; harness exclusion dropped from pre-push.
- [x] **HIGH** · _Performance_ — SPEC-03 WCOJ 4-cycle meets ≥10× gate ([#1](https://github.com/sunstoneinstitute/horndb/issues/1)) — ~34× on the canonical skewed win case (`SyntheticGraph::skewed_four_cycle`).
- [x] **HIGH** · _Performance_ — GraphBLAS closure backend wired + injectable into the owlrl Engine ([#61](https://github.com/sunstoneinstitute/horndb/issues/61)); profiling shows the LUBM timing gate is `rdf:type`-scan-bound (#2/#39), not closure-bound.
- [x] **HIGH** · _Completeness_ — Workspace migrated to oxrdf 0.3 + end-to-end RDF 1.2 triple-term support (`<<( s p o )>>`, gated by `SparqlConfig::rdf12`).
- [x] **HIGH** · _Conformance_ — W3C RDF 1.2 N-Triples syntax subset (`rdf12-n-triples`, 4 positive + 6 negative) in `harness/selected.toml`.
- [x] **HIGH** · _Completeness_ — SPEC-07 SPARQL aggregation (`GROUP BY`/`COUNT`/`SUM`) + expanded `FILTER`/`BIND`/`IF` expressions (trainmarks-blocking) ([#66](https://github.com/sunstoneinstitute/horndb/issues/66))
- [x] **HIGH** · _Completeness_ — SPEC-07 wire SPARQL frontend onto real storage + WCOJ + materialized closure (trainmarks-blocking) ([#67](https://github.com/sunstoneinstitute/horndb/issues/67))
- [x] **HIGH** · _Completeness_ — SPEC-07 pattern-based Update (`INSERT`/`DELETE … WHERE`) (trainmarks-blocking) ([#51](https://github.com/sunstoneinstitute/horndb/issues/51))
- [x] **MEDIUM** · _Performance_ — SPEC-04 eq-rep-p skew ([#2](https://github.com/sunstoneinstitute/horndb/issues/2)) — class-canonical union-find pass (`eq_rep_p_opt.rs`), default `Optimized`; downstream `rdf:type` partition-by-class-id (F5) remains under #39.
- [x] **MEDIUM** · _Conformance_ — W3C OWL 2 RL test-suite ingestion pipeline (`harness extract-owl2-rl`; 91 cases → 78 green in `[suites.owl2-w3c-rl]`, reds in `KNOWN-MANIFEST-BUGS.md`).
- [x] **MEDIUM** · _Completeness_ — SPEC-02 storage (HDT cold tier, CXL/NVMe tiering, MVCC, …) ([#3](https://github.com/sunstoneinstitute/horndb/issues/3))
- [x] **MEDIUM** · _Completeness_ — SPEC-04 rules (`dt-*`, `cls-maxc*`, F5 skew, …) ([#4](https://github.com/sunstoneinstitute/horndb/issues/4))
- [x] **MEDIUM** · _Completeness_ — SPEC-05 closure (retraction path, GPU backend, LAGraph) ([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
- [x] **MEDIUM** · _Completeness_ — SPEC-06 incremental (retraction, MVCC) ([#6](https://github.com/sunstoneinstitute/horndb/issues/6))
- [x] **MEDIUM** · _Completeness_ — SPEC-07 SPARQL (property paths, full `Update`, GSP, `EXPLAIN`, …) ([#7](https://github.com/sunstoneinstitute/horndb/issues/7))
- [x] **MEDIUM** · _Completeness_ — SPEC-08 ML (HTTP boundary delivered; FAISS candidate generator deferred) ([#8](https://github.com/sunstoneinstitute/horndb/issues/8))
- [x] **MEDIUM** · _Completeness_ — SPEC-10 rdflib-compatible Python API (PyO3 bindings) ([#9](https://github.com/sunstoneinstitute/horndb/issues/9))
- [x] **MEDIUM** · _Conformance_ — SPEC-01 harness (full W3C/ORE/LDBC/UOBM suites; LUBM RDFox A/B wired at N=1) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
- [x] **MEDIUM** · _Performance_ — Closure valued-reasoning readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11))
- [x] **MEDIUM** · _Performance_ — Valued-closure / custom-semiring acceleration ([#12](https://github.com/sunstoneinstitute/horndb/issues/12))
- [x] **MEDIUM** · _Tooling_ — Speed up integration test runs (parallelize and/or consolidate test targets) ([#108](https://github.com/sunstoneinstitute/horndb/issues/108))
- [x] **LOW** · _Operational_ — GraphDB Free A/B reference: per-run bring-up (supersedes systemd unit) ([#126](https://github.com/sunstoneinstitute/horndb/issues/126))
- [x] **LOW** · _Tooling_ — tasks.sh portability on macOS (flock / gawk match / GNU date) ([#78](https://github.com/sunstoneinstitute/horndb/issues/78))
- [x] **LOW** · _Tooling_ — Vendored SuiteSparse:GraphBLAS as a git submodule (`v10.3.0`, static, OpenMP, checked-in bindings); supersedes the runner-install task.
- [x] **LOW** · _Maintainability_ — Consolidated `selected.toml` into the single root file (`[sparql_query]` table).
- [x] **LOW** · _Maintainability_ — Plans/specs cross-reference cleanup (`docs/specs/README.md` Plan column).
- [x] **LOW** · _Tooling_ — CI installs GraphBLAS on runners (superseded by the vendored submodule above).
- [x] **LOW** · _Completeness_ — Wired `horndb_owlrl::Engine` to satisfy the harness `Reasoner` trait.

### Archive — project bootstrap

- [x] 9 specs written (SPEC-00..09); 9 plans (one per spec; SPEC-09 roadmap-only).
- [x] 7 implementation subagents dispatched in parallel under worktree isolation; all landed signed commits into main.
