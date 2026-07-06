---
status: executed
date: 2026-06-29
scope: "Metrics Phase 2 — Slice 3 (ml)"
---

# Metrics Phase 2 — Slice 3 (ml) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Checkbox steps.

**Goal:** Instrument the ML/LLM boundary (`horndb-ml`, SPEC-08) with Prometheus metrics: NL-query counts by result, LLM token usage + estimated cost, and translate / execute / audit-query latency.

**Architecture:** New `crates/metrics/src/ml.rs` (`MlMetrics`) registered into `MetricsState` (always compiled). The emit sites live in `horndb-ml`'s `server` module, which is behind the crate's existing `server` feature — so `horndb-metrics` is added as an **optional** dep wired into the `server` feature (mirroring how `axum`/`tokio` are gated). One label value enum (`NlResult`) for the `nl_query_total{result}` counter; everything else unlabelled.

**Tech Stack:** Rust 1.90, prometheus-client 0.25, axum/tokio (behind `server`), `horndb-metrics`, `horndb-ml`.

**Reference spec:** `docs/specs/SPEC-18-metrics.md` §7.2 (ml). Epic: #148.

**Branch:** `feat/metrics-phase2-ml`, stacked on `feat/metrics-phase2-incremental`.

---

## Metric inventory (names omit `horndb_`)

| Metric | Type | Source |
|---|---|---|
| `ml_nl_query_total{result}` | Counter (Family, label `result` ∈ {ok,error}) | end of `handle_nl_query` |
| `ml_prompt_tokens_total` | Counter (u64) | `CostJson.prompt_tokens` |
| `ml_completion_tokens_total` | Counter (u64) | `CostJson.completion_tokens` |
| `ml_estimated_usd_total` | Counter\<f64\> | `CostJson.estimated_usd` |
| `ml_translate_duration_seconds` | Histogram | around `translator.translate()` |
| `ml_execute_duration_seconds` | Histogram | around `executor.execute()` |
| `ml_audit_query_duration_seconds` | Histogram | around `query_since()` in `handle_ml_audit` |

§5.3: per-call timing only (translate / execute / audit query are whole-call spans).

---

## File Structure

- `crates/metrics/src/labels.rs` — add `NlResult` value enum + `NlResultLabel` set.
- `crates/metrics/src/ml.rs` — **new** `MlMetrics` + `register`.
- `crates/metrics/src/lib.rs` — wire `ml` into `MetricsState`.
- `crates/ml/Cargo.toml` — add `horndb-metrics` as optional dep in the `server` feature.
- `crates/ml/src/server/nlquery.rs` — emit translate/execute latency, token/cost counters, nl_query result counter.
- `crates/ml/src/server/audit.rs` — emit audit-query latency.
- Tests: inline test in `ml.rs`; ml server tests (`--features server`) exercising the handlers.

---

## Task 1: ml label types + `MlMetrics` subsystem

**Files:** `crates/metrics/src/labels.rs`, `crates/metrics/src/ml.rs`, `crates/metrics/src/lib.rs`.

- [ ] **Step 1:** In `labels.rs` add `label_value_enum!(NlResult { Ok => "ok", Error => "error" });` and `#[derive(...EncodeLabelSet)] pub struct NlResultLabel { pub result: NlResult }`.
- [ ] **Step 2:** Create `ml.rs` mirroring `owlrl.rs`. `MlMetrics` fields: `nl_query: Family<NlResultLabel, Counter>`, `prompt_tokens: Counter`, `completion_tokens: Counter`, `estimated_usd: Counter<f64>` (use `prometheus_client::metrics::counter::Counter`; the f64 variant is `Counter::<f64>::default()` — confirm the type param compiles and registers), `translate_duration_seconds: Histogram`, `execute_duration_seconds: Histogram`, `audit_query_duration_seconds: Histogram`. Register with the inventory names. Inline test: observe one of each, encode, assert `horndb_ml_nl_query_total`, `result="ok"`, `horndb_ml_prompt_tokens_total`, `horndb_ml_estimated_usd_total`, `horndb_ml_translate_duration_seconds` present.
- [ ] **Step 3:** Wire `ml` into `lib.rs` (`pub mod ml;`, field, register).
- [ ] **Step 4:** `cargo nextest run -p horndb-metrics` PASS; clippy clean.
- [ ] **Step 5:** Commit `feat(metrics): add MlMetrics subsystem + NlResult label`.

## Task 2: Emit from `handle_nl_query`

**Files:** `crates/ml/Cargo.toml`, `crates/ml/src/server/nlquery.rs`; ml server tests.

> Anchors (verify): `handle_nl_query` (nlquery.rs:84), `translator.translate(&q)` (:108), `CostJson { ... }` populated (:152-157), `executor.execute(...)` (:168). The handler is `#[cfg(feature = "server")]`.

- [ ] **Step 1:** In `crates/ml/Cargo.toml`: declare `horndb-metrics = { workspace = true, optional = true }` and add `"dep:horndb-metrics"` to the `server` feature's list (mirror how `dep:axum`/`dep:tokio` are listed).
- [ ] **Step 2 (failing test):** Find an existing ml server test (likely uses a fake/stub translator + executor) and copy its setup. Add a test that drives `handle_nl_query` (or the in-process path it wraps) once and asserts `horndb_metrics::encode_metrics()` contains `horndb_ml_nl_query_total` with `result="ok"` and that `horndb_ml_translate_duration_seconds_count` parses `>= 1`. Run with `cargo nextest run -p horndb-ml --features server nlquery` (or the matching test path) — confirm FAIL. If the handler is hard to exercise directly (needs a live LLM), instrument and test the smallest in-process function that the handler calls and that the stub translator can drive; report what you chose.
- [ ] **Step 3:** Wrap `translator.translate(&q)` with an `Instant`, observe `translate_duration_seconds`. Wrap `executor.execute(...)` similarly → `execute_duration_seconds`. After the `CostJson` is populated, `prompt_tokens.inc_by(cost.prompt_tokens)`, `completion_tokens.inc_by(cost.completion_tokens)`, `estimated_usd.inc_by(cost.estimated_usd)`. At the handler's terminal points, increment `nl_query.get_or_create(&NlResultLabel{result: NlResult::Ok|Error}).inc()` — `Ok` on the success path, `Error` on each error path. Guard against double-count (count the result exactly once per request). All emit calls are inside the `#[cfg(feature="server")]` module, so reference `horndb_metrics::` directly.
- [ ] **Step 4:** `cargo nextest run -p horndb-ml --features server` PASS; `cargo clippy -p horndb-ml --all-targets --features server -- -D warnings` clean. Also confirm `cargo build -p horndb-ml` (no `server`) still builds (the optional dep must not be referenced outside the feature).
- [ ] **Step 5:** Commit `feat(metrics): instrument ml nl-query (latency/tokens/cost/result)`.

## Task 3: Emit audit-query latency

**Files:** `crates/ml/src/server/audit.rs`; extend ml server tests.

> Anchors: `handle_ml_audit` (audit.rs:74), `query_since(...)` (:118).

- [ ] **Step 1 (failing test):** Extend the ml server test to drive the audit path (or the function wrapping `query_since`) and assert `horndb_ml_audit_query_duration_seconds_count >= 1`. Confirm FAIL.
- [ ] **Step 2:** Wrap the `query_since(...)` call with an `Instant`/observe into `audit_query_duration_seconds`.
- [ ] **Step 3:** `cargo nextest run -p horndb-ml --features server` PASS; clippy (with `--features server`) clean.
- [ ] **Step 4:** Commit `feat(metrics): instrument ml audit-query latency`.

## Task 4: Docs sync + verification

**Files:** `docs/architecture.md` (§15), `TASKS.md`, `docs/index.md`.

- [ ] **Step 1:** architecture.md §15 — ml → implemented; remaining = wcoj, sparql-bytes.
- [ ] **Step 2:** TASKS.md — mark ml fan-out done; add Slice-3 landed note. No GitHub issue edits.
- [ ] **Step 3:** docs/index.md — add pointer to this plan if it enumerates plans; commit the plan file.
- [ ] **Step 4:** `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo nextest run -p horndb-metrics -p horndb-ml --features server` (note: include `--features server` so the ml emit paths compile/test). All clean/PASS.
- [ ] **Step 5:** Commit `docs(metrics): record Phase-2 ml slice (#148)`.

---

## Self-Review checklist
- §7.2 ml coverage: nl_query{result} ✓, prompt/completion tokens ✓, estimated_usd ✓, translate/execute latency ✓, audit-query latency ✓.
- §5.3: per-call timing only. ✓
- `nl_query{result}` label bounded (ok/error). ✓
- Optional dep gated under `server`; non-server build still compiles. ✓
- `nl_query` result counted exactly once per request (both ok and error paths). ✓

## Execution handoff
subagent-driven-development; stacked PR against `feat/metrics-phase2-incremental`; do not merge; tick #148 ml box when green.
