# Metrics Phase 2 — Slice 2 (incremental) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Instrument the DBSP-style incremental maintenance circuit (`horndb-incremental`) with Prometheus metrics: per-tick latency, asserted/derived merge cardinalities, closure retract/promote cardinalities, fixpoint rounds per tick, and a change-feed subscriber gauge.

**Architecture:** Mirror the established subsystem pattern. A new `crates/metrics/src/incremental.rs` defines `IncrementalMetrics` (all unlabelled counters/histograms/gauge — no new label types) registered into `MetricsState`. The circuit emits per-tick metrics at the single tick finalization point (`TickReport` construction); the change-feed gauge is `set()` to the live subscriber count at subscribe and at publish-time reaping.

**Tech Stack:** Rust 1.90, prometheus-client 0.25, `horndb-metrics`, `horndb-incremental`.

**Reference spec:** `docs/specs/2026-06-29-metrics-design.md` §7.2 (incremental), §5.3 (cost boundary). Epic: #148.

**Branch:** `feat/metrics-phase2-incremental`, stacked on `feat/metrics-phase2-owlrl`.

---

## Metric inventory (names omit the `horndb_` prefix; the registry adds it)

| Metric | Type | Source |
|---|---|---|
| `incremental_tick_duration_seconds` | Histogram (per tick) | `Instant` around `tick()` |
| `incremental_asserted_merged_total` | Counter | `TickReport.asserted_merged` |
| `incremental_derived_merged_total` | Counter | `TickReport.derived_merged` |
| `incremental_closure_withdraw_total` | Counter | sum of `ClosureRetractDelta.withdraw.len()` per tick |
| `incremental_closure_promote_total` | Counter | sum of `ClosureRetractDelta.promote.len()` per tick |
| `incremental_fixpoint_rounds` | Histogram (per tick) | rounds actually run in the fixpoint loop |
| `incremental_change_feed_subscribers` | Gauge | `ChangeFeed` subscriber count |

All unlabelled. No per-tuple timing — all timing is per-tick (§5.3 compliant).

---

## File Structure

- `crates/metrics/src/incremental.rs` — **new**: `IncrementalMetrics` + `register`.
- `crates/metrics/src/lib.rs` — add `pub mod incremental;`, field, register call.
- `crates/incremental/Cargo.toml` — add `horndb-metrics.workspace = true`.
- `crates/incremental/src/circuit.rs` — time the tick; accumulate withdraw/promote; count fixpoint rounds; emit all per-tick metrics at the `TickReport` finalization.
- `crates/incremental/src/change_feed.rs` — `set()` the subscriber gauge at subscribe + publish reap.
- Tests: inline `#[cfg(test)]` in `incremental.rs`; `crates/incremental/tests/metrics.rs` (new) exercising a tick + a subscribe.

---

## Task 1: `IncrementalMetrics` subsystem

**Files:** Create `crates/metrics/src/incremental.rs`; modify `crates/metrics/src/lib.rs`.

- [ ] **Step 1:** Create `incremental.rs` mirroring `owlrl.rs`/`sparql.rs`. Use `Counter`, `Histogram` (`latency_hist()` = `exponential_buckets(1e-4, 3.0, 12)` for tick duration; a count-style bucket e.g. `exponential_buckets(1.0, 2.0, 10)` for `fixpoint_rounds`), and `Gauge` (`prometheus_client::metrics::gauge::Gauge`, default `i64`). Fields: `tick_duration_seconds: Histogram`, `asserted_merged: Counter`, `derived_merged: Counter`, `closure_withdraw: Counter`, `closure_promote: Counter`, `fixpoint_rounds: Histogram`, `change_feed_subscribers: Gauge`. Register each with the names in the inventory (counters get `_total` automatically from the encoder; register base names `incremental_asserted_merged` etc.). Add an inline test asserting the encoded registry contains `horndb_incremental_tick_duration_seconds` and `horndb_incremental_change_feed_subscribers` after observing once.
- [ ] **Step 2:** Wire into `lib.rs` (`pub mod incremental;`, `pub incremental: incremental::IncrementalMetrics` field, register in `MetricsState::new()`).
- [ ] **Step 3:** `cargo nextest run -p horndb-metrics` — PASS.
- [ ] **Step 4:** Commit `feat(metrics): add IncrementalMetrics subsystem`.

## Task 2: Emit per-tick metrics from the circuit

**Files:** `crates/incremental/Cargo.toml`, `crates/incremental/src/circuit.rs`; test `crates/incremental/tests/metrics.rs`.

> Verify line numbers before editing. Anchors: `pub fn tick(&mut self) -> TickReport`; the fixpoint `for _ in 0..MAX_ROUNDS` loop; the closure retract pass destructuring `ClosureRetractDelta { withdraw, promote }`; the single `TickReport { asserted_merged, derived_merged, logical_time }` tail construction.

- [ ] **Step 1:** Add `horndb-metrics.workspace = true` to `crates/incremental/Cargo.toml`.
- [ ] **Step 2 (failing test):** `crates/incremental/tests/metrics.rs` — copy tick setup from an existing circuit test that performs an assert + `tick()`, then assert `horndb_metrics::encode_metrics()` contains `horndb_incremental_tick_duration_seconds` and that `horndb_incremental_asserted_merged_total` parses to a value (use a `parse_counter` helper like the owlrl test). Run, confirm FAIL.
- [ ] **Step 3:** At `tick()` start, `let t_tick = std::time::Instant::now();`. Change the fixpoint loop `for _ in 0..MAX_ROUNDS` → `for round in 0..MAX_ROUNDS` and track rounds actually run (e.g. `let mut rounds_run = 0usize;` incremented each iteration, captured before the `break`). In the closure retract pass, accumulate `let mut withdraw_n = 0u64; let mut promote_n = 0u64;` (`+= withdraw.len() as u64` / `+= promote.len() as u64`).
- [ ] **Step 4:** Immediately before the `TickReport { .. }` construction (or right after, using the report's fields), emit:
  ```
  let m = horndb_metrics::metrics();
  m.incremental.tick_duration_seconds.observe(t_tick.elapsed().as_secs_f64());
  m.incremental.asserted_merged.inc_by(asserted_merged as u64);
  m.incremental.derived_merged.inc_by(derived_merged as u64);
  m.incremental.closure_withdraw.inc_by(withdraw_n);
  m.incremental.closure_promote.inc_by(promote_n);
  m.incremental.fixpoint_rounds.observe(rounds_run as f64);
  ```
  Ensure `tick()` has a single return path (or emit on all). If the closure retract pass only runs for retraction ticks, `withdraw_n`/`promote_n` stay 0 for assertion-only ticks — that is correct.
- [ ] **Step 5:** `cargo nextest run -p horndb-incremental` PASS; `cargo clippy -p horndb-incremental --all-targets -- -D warnings` clean.
- [ ] **Step 6:** Commit `feat(metrics): instrument incremental tick (latency/merges/retract/rounds)`.

## Task 3: Change-feed subscriber gauge

**Files:** `crates/incremental/src/change_feed.rs`; extend `crates/incremental/tests/metrics.rs`.

> Anchors: `subscribe()` (pushes a new tx); the publish path doing `subs.retain(|tx| tx.send(rec).is_ok())`; `subscriber_count()` getter.

- [ ] **Step 1 (failing test):** Extend `tests/metrics.rs`: create a `ChangeFeed`, call `subscribe()`, assert `horndb_metrics::encode_metrics()` contains `horndb_incremental_change_feed_subscribers` with value `>= 1`. Run, confirm FAIL.
- [ ] **Step 2:** After the `subscribe()` push (while still holding the write lock, or immediately after) `set` the gauge to the new subscriber count: `horndb_metrics::metrics().incremental.change_feed_subscribers.set(self.subscribers.read()...len() as i64);` — use whatever lock accessor the code already uses; simplest is to `set` to the post-mutation length you already have. After the publish `retain`, `set` the gauge to the new (possibly smaller) length so reaped dead subscribers are reflected.
- [ ] **Step 3:** `cargo nextest run -p horndb-incremental` PASS; clippy clean.
- [ ] **Step 4:** Commit `feat(metrics): change-feed subscriber gauge`.

## Task 4: Docs sync + verification

**Files:** `docs/architecture.md` (§15), `TASKS.md`, `docs/index.md`.

- [ ] **Step 1:** architecture.md §15 — move incremental from planned → implemented; leave ml/wcoj/sparql-bytes planned.
- [ ] **Step 2:** TASKS.md — mark the incremental fan-out item done; add this plan to the landed list. Do not touch the GitHub issue (controller mirrors #148).
- [ ] **Step 3:** docs/index.md — add a pointer to this plan if the index enumerates plans.
- [ ] **Step 4:** `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo nextest run -p horndb-metrics -p horndb-incremental`. All clean/PASS.
- [ ] **Step 5:** Commit `docs(metrics): record Phase-2 incremental slice (#148)`.

---

## Self-Review checklist
- §7.2 incremental coverage: tick latency ✓, asserted/derived merged ✓, retract/promote ✓, fixpoint rounds ✓, subscriber gauge ✓.
- §5.3: only per-tick timing; no per-tuple. ✓
- No new label types (all unlabelled) — bounded by construction. ✓
- Single tick finalization emit (no double/missing). ✓
- Gauge reflects reaped subscribers (set after retain). ✓

## Execution handoff
subagent-driven-development; stacked PR against `feat/metrics-phase2-owlrl`; do not merge; tick #148 incremental box when green.
