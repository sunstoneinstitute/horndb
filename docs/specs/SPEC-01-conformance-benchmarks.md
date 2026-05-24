# SPEC-01 — Conformance & Benchmarking Harness

## Purpose

The harness that decides whether the engine is correct (W3C tests, ORE 2015) and whether it is competitive (LDBC SPB, LUBM, UOBM, real-world ontologies). All correctness claims and all performance numbers in marketing/papers/sales contexts must be reproducible from this harness.

A bug here is a project-existential bug — if we cannot trust the benchmark, we cannot trust the engine.

**This spec is intentionally numbered first.** The harness is built before the engine it grades, so that every subsequent SPEC's acceptance criteria can name a specific subset of the standard suites and expect them to be green in CI. We do **not** need to run every test from day one; we **do** need to run *something* meaningful from day one and grow the selected subset as the engine grows. Whatever subset is currently selected must pass — selection is a versioned project artifact (see F11), not a per-developer choice.

## Scope

In scope:
- The four tiers of test suites named in the research:
  1. **Correctness (normative):** W3C OWL 2 Test Cases, W3C SPARQL 1.1 Test Suite (including Entailment Regimes).
  2. **Correctness (stress):** ORE 2015 corpus (Zenodo record 18578, 1,920 ontologies + 47 user submissions).
  3. **Performance (primary):** LDBC Semantic Publishing Benchmark v2.0, SF3 and SF5.
  4. **Performance (profile coverage):** LUBM (LUBM-100, LUBM-1000, LUBM-8000) and UOBM (UOBM-DL).
- Real-world ontology stress: SNOMED CT (EL), Gene Ontology, UniProt subset, Reactome, ChEMBL.
- Pure-SPARQL regression suites: WatDiv, BSBM, SP²Bench.
- A CI runner that runs the correctness tier on every commit; the performance tier on tagged releases and weekly on `main`.
- Result storage and trend tracking: every run records `(commit-sha, suite, hardware, throughput-metric, latency-metric)` to a queryable backing store (likely SQLite for Stage 1; ClickStack later).
- A differential-comparison harness: A/B against RDFox (where licensable for benchmarking) and GraphDB Free, on identical hardware.

Out of scope:
- The competitor licenses themselves (procurement is operational, not architectural).
- Publication-grade graphing — we just need raw numbers; charts can come later.

## Functional requirements

**F1. W3C OWL 2 Test Cases runner.** Parse the W3C-provided test-case manifests (RDF/XML), execute each test case against the engine, classify as `passed` / `failed` / `skipped` (with reason), produce a per-suite report. The harness must recognise positive entailment, negative entailment, consistency, inconsistency, and reasoning-correctness test types.

**F2. W3C SPARQL 1.1 Test Suite runner.** Same shape, parsing the SPARQL 1.1 manifest. Includes the Entailment Regimes sub-suite, run under the OWL 2 RL/RDF regime.

**F3. ORE 2015 runner.** Wrap the upstream ORE 2015 competition framework (github.com/ykazakov/ore-2015-competition-framework). Run consistency, classification, and realisation tasks over the 1,920-ontology corpus. Time budget per ontology: 5 min wall clock (matches ORE 2015 competition rules).

**F4. LDBC SPB driver integration.** Integrate the upstream LDBC SPB v2.0 driver. SF3 (~256M triples post-expansion) and SF5 (~1B edges) as headline runs. Capture both editorial-mix throughput (queries/sec) and update-throughput.

**F5. LUBM/UOBM driver.** Generate LUBM datasets at SF=100, 1000, 8000 using the upstream UBA (University Benchmark Application). Run the 14 LUBM queries. UOBM-DL similarly with its query mix.

**F6. Real-world ontology suite.** Provided as a curated set of `(name, source-URL, version, expected-triple-count, expected-class-count, smoke-query)` tuples in `harness/realworld.toml`. Smoke queries probe known answer sets — used as differential references against RDFox / ELK / GraphDB.

**F7. Result database.** Each run produces `(run_id, commit_sha, hardware_id, suite, dataset, metric_name, metric_value, units, timestamp)` rows. Hardware ID is a fingerprint of `(cpu_model, mem_size, mem_speed, gpu_model)`. Stored in SQLite for Stage 1.

**F8. Trend reports.** A `harness report --suite=spb-sf3 --metric=geomean-latency --since=30d` query returns the time series for that metric on that suite over the last 30 days, with a regression flag (>20% slowdown vs 7-day median).

**F9. CI integration.** Correctness-tier runs as a required check on every PR. Performance-tier runs nightly on the dedicated benchmark machine and posts a summary to the project's Slack/email.

**F10. Comparison runs.** When a competitor binary is available (RDFox under a benchmarking license; GraphDB Free), the harness can run the identical workload against the competitor and tabulate the comparison. Same hardware fingerprint constraint.

**F11. Selected subset manifest.** A versioned file `harness/selected.toml` declares, for each suite, the exact list of test IDs that are currently "in" — i.e., expected to pass. CI runs only the selected subset on each PR. Adding new tests to `selected.toml` is a normal PR change (and the engine must be passing them when the PR lands). Removing tests requires an `xfail-reason` comment with a tracking issue. The selected subset only grows monotonically over time, except for documented removals.

**F12. Stub-engine smoke target.** Before the real engine exists, the harness must be runnable against a stub implementation (a tiny in-memory store that fails on any non-trivial query). This proves the harness itself works — when the stub fails a test, it must be flagged red. This is how we test the test bench.

## Non-functional requirements

**NF1. Correctness-tier runtime.** Full W3C OWL 2 + SPARQL 1.1 test suites complete in ≤10 min on the reference workstation. This is the CI budget; if it exceeds, PRs back up.

**NF2. ORE 2015 corpus runtime.** Full corpus run completes in ≤8 hours (overnight). Per-ontology budget 5 minutes — most finish in <1 s.

**NF3. SPB driver fidelity.** SPB driver runs are LDBC-audit-grade — meaning we follow the LDBC audit checklist (deterministic warm-up, sustained 1-hour measurement, no in-flight schema changes) and our results would survive an external auditor. Whether we pursue formal audit is a Stage 3+ decision.

**NF4. Reproducibility.** Re-running the same suite at the same commit on the same hardware produces results within 5% of the original. Variance beyond 5% is a harness defect to be investigated.

**NF5. Differential testing.** When we have a competitor reference (RDFox or GraphDB) for an answer set, the harness flags any divergence in result set as a critical defect.

## Dependencies

- **None on other SPECs.** This is the first spec; it is built against a stub engine and the real engine slots in later. Subsequent specs depend on *this* one for their acceptance criteria.
- Upstream: W3C test suite tarballs, ORE 2015 competition framework, LDBC SPB driver, LUBM UBA, UOBM generator, public ontology mirrors.
- Optionally: RDFox benchmarking-license binary, GraphDB Free binary, ELK jar.

## Acceptance criteria

Acceptance is staged. The harness gets more capable as the engine does; the rule is that whatever is *selected* in `harness/selected.toml` at a given commit must be 100% green.

**Stage 0 (harness bootstrap, 2–4 weeks):**
1. Runner exists for W3C OWL 2 Test Cases and SPARQL 1.1 Test Suite. Can parse manifests, dispatch, classify pass/fail/skip.
2. `harness/selected.toml` exists and selects ≥1 test from each suite. CI runs the selected subset on every PR.
3. A stub engine fails its assigned tests; CI correctly turns red. (F12 — proving the harness works.)
4. Result database (SQLite) wired up; `harness report` returns rows for the stub runs.

**Stage 1 (feasibility prototype, 3 months):**
5. Selected W3C OWL 2 RL subset expanded to ≥50 test cases covering the most-used rules. All selected tests pass.
6. ORE 2015 runner integrated (F3); selected subset starts at a hand-picked 10 ontologies known to be OWL 2 RL clean.
7. LDBC SPB-256 runs end-to-end against the real engine and against GraphDB Free for comparison (F10).

**Stage 2 (MVP, 12 months):**
8. Selected subset expanded to the *full* W3C OWL 2 RL test cases and the *full* SPARQL 1.1 Entailment Regimes (OWL 2 RL/RDF) suite. All passing.
9. ORE 2015 OWL 2 RL fragment runs to completion with 100% solved within per-ontology 5-min budget.
10. LDBC SPB SF3 audited-style report published internally; comparison to GraphDB Enterprise documented.
11. LUBM-8000 materialization run published with hardware fingerprint and per-stage timings.
12. Differential A/B harness functional on at least one competitor (GraphDB Free is the minimum viable target — RDFox license depends on procurement).

**Continuous (every stage):**
- CI runs the currently-selected correctness subset on every PR in ≤10 minutes. PR is blocked on any failure.
- Performance tier runs nightly on the dedicated benchmark machine and posts a summary.
- `harness report --since=30d` returns trends with regression flagging.

## Risks and open questions

- **W3C test-case ambiguity.** Some W3C tests are themselves under-specified or have known bugs in the manifest. Strategy: maintain a `KNOWN-MANIFEST-BUGS.md` file documenting each waiver, with a citation to the W3C tracker.
- **ORE 2015 corpus drift.** The Zenodo corpus is frozen but some ontologies in it reference external imports that may 404. We snapshot all import closure into the harness repo.
- **LDBC SPB licensing.** SPB driver is Apache 2.0 (LDBC). Free to use; audit programme is a separate process.
- **Competitor licensing.** RDFox commercial licenses typically forbid published comparative benchmarks ("DeWitt clause"). We will need legal review before *publishing* any RDFox comparison numbers. Internal use is generally permitted; that is the minimum viable use case for Stage 1.
- **Hardware fingerprint normalisation.** Different cloud SKUs, different microcode, different DRAM populations all affect benchmarks. We normalise by capturing the fingerprint, not by trying to normalise across hardware — comparisons are valid only within identical fingerprints.
- **Continuous LDBC SPB SF5 in CI is expensive.** Plan: SF5 weekly, SF3 nightly, SPB-256 per-PR (if it fits the 10-min budget). Decision deferred until we have first-pass timing data.
- **Real-world ontologies move.** SNOMED CT, UniProt, etc. have versioned releases. Pin specific versions in `realworld.toml` and re-download deterministically.
