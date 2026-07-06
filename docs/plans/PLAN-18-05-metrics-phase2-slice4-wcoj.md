---
status: executed
date: 2026-06-29
scope: "Metrics Phase 2 — Slice 4 (wcoj)"
---

# Metrics Phase 2 — Slice 4 (wcoj) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Checkbox steps.

**Goal:** Instrument the leapfrog-triejoin executor (`horndb-wcoj`, SPEC-03) with **developer** metrics at **whole-query granularity only**: seeks per query, leapfrog iterations per query, and peak active iterators. **No per-seek / per-tuple timing** (§5.3) — the inner loop only increments plain local integer counters; the metrics histograms are observed exactly once when the query iterator is dropped.

**Architecture:** New `crates/metrics/src/wcoj.rs` (`WcojMetrics`, three histograms, unlabelled) registered into `MetricsState`. `BatchIter` gains two `u64` accumulators (`seeks`, `iterations`) bumped at the seek/leapfrog sites — these are ordinary struct-field increments, NOT metric calls, so the hot path stays free of atomic/Instant cost. A `Drop for BatchIter` impl observes the three histograms once per query (covers both fully-consumed and early-terminated/LIMIT queries, exactly once, no double-count). Peak iterators = `self.iters.len()` (the `iters` vec is fixed after construction).

**Tech Stack:** Rust 1.90, prometheus-client 0.25, `horndb-metrics`, `horndb-wcoj`.

**Reference spec:** `docs/specs/SPEC-18-metrics.md` §7.2 (wcoj), §5.3 (the per-tuple prohibition — this slice is the canonical place that boundary matters). Epic: #148.

**Branch:** `feat/metrics-phase2-wcoj`, stacked on `feat/metrics-phase2-ml`.

---

## Metric inventory (names omit `horndb_`; all unlabelled, all observed once per query in `Drop`)

| Metric | Type | Definition |
|---|---|---|
| `wcoj_seeks_per_query` | Histogram | total `.seek(...)` calls made by the query |
| `wcoj_iterations_per_query` | Histogram | total leapfrog convergence-loop iterations in `find_match` |
| `wcoj_peak_iterators` | Histogram | `iters.len()` (number of trie iterators / BGP patterns) |

Bucket suggestion: `exponential_buckets(1.0, 4.0, 12)` (covers 1 → ~4M) for seeks/iterations; `exponential_buckets(1.0, 2.0, 12)` for peak iterators.

---

## File Structure

- `crates/metrics/src/wcoj.rs` — **new** `WcojMetrics` + `register`.
- `crates/metrics/src/lib.rs` — wire `wcoj` into `MetricsState`.
- `crates/wcoj/Cargo.toml` — add `horndb-metrics.workspace = true`.
- `crates/wcoj/src/executor/wcoj.rs` — add `seeks`/`iterations` fields to `BatchIter`, bump them at the seek / leapfrog-iteration sites, add `Drop for BatchIter` that observes the three histograms.
- Tests: inline in `wcoj.rs` (metrics crate); `crates/wcoj/tests/metrics.rs` (new) driving a BGP query to completion.

---

## Task 1: `WcojMetrics` subsystem

**Files:** `crates/metrics/src/wcoj.rs`, `crates/metrics/src/lib.rs`.

- [ ] **Step 1:** Create `wcoj.rs` mirroring `owlrl.rs`. Three `Histogram` fields (`seeks_per_query`, `iterations_per_query`, `peak_iterators`) with the buckets above. Register with the inventory names. Inline test: observe one of each, encode, assert the three series names present.
- [ ] **Step 2:** Wire into `lib.rs` (`pub mod wcoj;`, field, register).
- [ ] **Step 3:** `cargo nextest run -p horndb-metrics` PASS; clippy clean.
- [ ] **Step 4:** Commit `feat(metrics): add WcojMetrics subsystem`.

## Task 2: Accumulate + emit from `BatchIter`

**Files:** `crates/wcoj/Cargo.toml`, `crates/wcoj/src/executor/wcoj.rs`; test `crates/wcoj/tests/metrics.rs`.

> Anchors (verify): `struct BatchIter<'src, S>` (~:169-210) with the `iters: Vec<AdaptiveIter<...>>` field; `find_match()` (~:445-481, the leapfrog convergence loop); `step()` (~:483-573); `.seek(...)` calls (~:436, :466). READ the whole `BatchIter` + `find_match` + `step` before editing.

- [ ] **Step 1:** Add `horndb-metrics.workspace = true` to `crates/wcoj/Cargo.toml` `[dependencies]`. (wcoj sits above metrics in the dep graph — no cycle.)
- [ ] **Step 2 (failing test):** Create `crates/wcoj/tests/metrics.rs`. Find an existing executor test that builds a `TripleSource` + runs a multi-pattern BGP query through `into_iter()`/`BatchIter` and consumes it. COPY that setup, consume the iterator fully, then assert `horndb_metrics::encode_metrics()` contains `horndb_wcoj_seeks_per_query`, `horndb_wcoj_peak_iterators`, and that `horndb_wcoj_peak_iterators_count >= 1` (parse). Run, confirm FAIL.
- [ ] **Step 3:** Add `seeks: u64` and `iterations: u64` fields to `BatchIter`, initialized to 0 wherever `BatchIter` is constructed.
- [ ] **Step 4:** Bump the accumulators at the correct sites (PLAIN field increments — do NOT call metrics here):
  - `self.seeks += 1;` at each `.seek(...)` call on a trie iterator inside `find_match`/`step`.
  - `self.iterations += 1;` once per iteration of the leapfrog convergence loop in `find_match`.
  Add a short `//` comment at each site defining what is counted, and make the help text match.
- [ ] **Step 5:** Add `impl Drop for BatchIter` (matching the generic params/bounds) that observes once:
  ```
  let m = horndb_metrics::metrics();
  m.wcoj.seeks_per_query.observe(self.seeks as f64);
  m.wcoj.iterations_per_query.observe(self.iterations as f64);
  m.wcoj.peak_iterators.observe(self.iters.len() as f64);
  ```
  Every `BatchIter` is dropped exactly once → exactly one observation per query, covering exhausted AND early-terminated (LIMIT) queries, with no double-count. Confirm `BatchIter` does not already impl `Drop` (if it does, fold the observe into it).
- [ ] **Step 6:** `cargo nextest run -p horndb-wcoj` PASS; `cargo clippy -p horndb-wcoj --all-targets -- -D warnings` clean. Run the wcoj differential fuzzer test if present to confirm no behavior change.
- [ ] **Step 7:** Commit `feat(metrics): wcoj per-query seeks/iterations/peak-iterators (no per-seek timing)`.

## Task 3: Docs sync + verification

**Files:** `docs/architecture.md` (§15), `TASKS.md`, `docs/index.md`.

- [ ] **Step 1:** architecture.md §15 — wcoj → implemented (note: developer histograms, whole-query granularity, no per-seek timing). Remaining = SPARQL response-bytes only.
- [ ] **Step 2:** TASKS.md — mark wcoj fan-out done; add Slice-4 landed note. Leave SPARQL-bytes open. No GitHub issue edits.
- [ ] **Step 3:** docs/index.md — add pointer to this plan; commit the plan file.
- [ ] **Step 4:** `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo nextest run -p horndb-metrics -p horndb-wcoj`. Clean/PASS. Commit any Cargo.lock change.
- [ ] **Step 5:** Commit `docs(metrics): record Phase-2 wcoj slice (#148)`.

---

## Self-Review checklist
- §7.2 wcoj coverage: seeks-per-query ✓, iterations-to-match ✓, peak active iterators ✓. (Ground-pattern pre-check pass rate is out of scope for this slice per the handoff's wcoj priority list.)
- **§5.3 (the critical one):** the inner loop only does `self.field += 1` (plain integer), NEVER a metric call or `Instant::now()`. The single metric `observe` is in `Drop`, once per query. Verify NO atomic/timer in any per-seek/per-tuple path.
- Exactly-once emission via `Drop`; covers early-terminated queries; no double-count.
- No behavior change to join results (fuzzer/existing tests green).

## Execution handoff
subagent-driven-development; stacked PR against `feat/metrics-phase2-ml`; do not merge; tick #148 wcoj box when green.
