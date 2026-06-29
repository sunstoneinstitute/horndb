# Metrics & Observability — Design (Phase 1: Metrics)

**Status:** specified
**Date:** 2026-06-29
**Scope:** In-process metrics for operators and developers, exported for
Prometheus scrape. Traces and logs are explicitly **out of scope** for this
phase.

## 1. Goal

Give HornDB two audiences first-class metrics:

1. **Operators (critical)** — resource consumption and health: cache/pool/dictionary
   sizes, triples loaded, bytes over the network, bytes to/from disk and (eventually)
   memory tiers (HBM / regular RAM / CXL), error rates, request rates.
2. **Developers (very useful)** — performance histograms (elapsed time per operation)
   so P50/P99/P999 latencies are observable for hot paths.

The instrumentation must be cheap enough to leave on in production: hot-path updates
are direct atomic operations, and quantities that are expensive to compute are pulled
lazily at scrape time rather than maintained continuously.

## 2. Non-goals (this phase)

- No OpenTelemetry traces or logs. (Deferred to a later phase.)
- No in-process OTLP **push** exporter. OTel interop, when wanted, is achieved by
  running an OpenTelemetry Collector that **scrapes** the `/metrics` endpoint
  (Prometheus receiver) and re-exports over OTLP off-box. Nothing in HornDB changes
  for that path.
- No per-tuple / per-`seek()` timing (see §5.3 — the histogram cost boundary).

## 3. Library decision

**`prometheus-client`** (the official Prometheus / OpenMetrics Rust client), exported
via a `/metrics` scrape endpoint.

Rationale — string label names are unacceptable; we require typed, codegen'd labels and
direct-atomic hot-path updates (the philosophy of
<https://github.com/stigsb/prometheus-cpp/>):

- `prometheus-client` gives typed label sets via `#[derive(EncodeLabelSet)]` /
  `#[derive(EncodeLabelValue)]`. Labels are types, checked at compile time, not strings.
- `Family<Labels, Metric>::get_or_create(&labels)` returns a handle that is **cached and
  incremented directly** (`.inc()` / `.observe()`), so a resolved hot-path handle is just
  an atomic op — no per-update map lookup.
- Export is `encode(&mut buf, &registry)` → OpenMetrics text. Scrape model.

Rejected alternatives:

- **OpenTelemetry SDK (metrics)** — attributes are dynamic `&[KeyValue]` resolved and
  allocated *per measurement*; no typed-label codegen. Disqualified for the hot path.
  Pushing typed metrics over OTLP would force us to materialize typed labels to strings
  on a background thread — complexity we avoid by letting the Collector do it off-box.
- **`metrics` (metrics-rs) facade** — string-keyed; near-zero overhead only via the noop
  recorder, and a naive hot-path update is a sharded-hashmap lookup + `Arc` clone unless
  handles are cached. Typed labels are not the model.

## 4. Crate architecture

A new foundational crate **`horndb-metrics`** at the bottom of the dependency graph
(below `storage`), holding:

- the `prometheus-client` dependency (kept out of every other crate's public surface),
- the typed label-set definitions (`#[derive(EncodeLabelSet)]`),
- the per-subsystem metric structs (counters, gauges, histograms),
- a **process-global registry behind `OnceLock`**, plus free accessors.

### 4.1 Access pattern (global `OnceLock`)

Hot-path code anywhere reaches a cached handle through a free accessor — no context
object threaded through call sites:

```rust
// in horndb-metrics
pub fn metrics() -> &'static Metrics { /* OnceLock init-once */ }

// at a call site (e.g. owlrl engine)
horndb_metrics::metrics().owlrl.rule_fires.inc();
```

This mirrors the prometheus-cpp global-registry ergonomics the user asked for. The
explicit-injected-registry alternative (more testable, but plumbing through `storage`
and `wcoj`) was considered and rejected for friction.

`Metrics` groups handles by subsystem (`metrics().storage`, `.owlrl`, `.sparql`, …).
Tests that need isolation construct a local `Metrics` directly rather than touching the
global.

### 4.2 Naming & units

Prometheus convention: `horndb_<subsystem>_<name>_<unit>` with base units
(`_seconds`, `_bytes`, `_total` for counters). Example:
`horndb_sparql_query_duration_seconds`, `horndb_storage_dictionary_terms`,
`horndb_owlrl_rule_fires_total`.

### 4.3 Feature gating

- The `horndb-metrics` crate is a small always-compiled dependency. Updating a handle is
  a single atomic op, so there is no compile-time on/off switch for instrumentation in
  the production crates.
- The **`/metrics` HTTP endpoint** lives behind the sparql crate's existing **`server`**
  feature (it is an axum route).
- A `metrics` cargo feature on `horndb-metrics` may gate any *scrape-time `Collector`*
  that is non-trivial to register, so benchmark builds can drop it; the default is on.

## 5. Overhead model

The user's explicit concern: balance the number of metrics and measurement frequency
against library overhead. Three rules:

### 5.1 Cheap events update inline

Counters and histograms for discrete events (rule fires, query count, request bytes) are
direct atomic ops at the event site. Negligible cost.

### 5.2 Expensive sizes are pulled at scrape time

Quantities that are O(n) to compute — dictionary size, tier bytes, partition counts,
Z-set cardinalities — are **not** maintained continuously. They are registered as a
scrape-time `Collector` that reads the live struct (`TierStats`, etc.) on demand.
Steady-state cost is zero; the numbers materialize only when Prometheus scrapes
(typically every 15–60 s).

### 5.3 The histogram cost boundary

A timing histogram costs an `Instant::now()` (~20 ns `clock_gettime`) plus a bucket
atomic. That is fine **around a whole query, a fixpoint round, a closure call, or an
HTTP request**. It is **too expensive per-tuple** in the leapfrog inner loop. The design
draws the line explicitly:

- **Yes:** per-query, per-update-tick, per-fixpoint-round, per-closure-call,
  per-HTTP-request, per-load.
- **No:** per-`seek()`, per-`next()`, per-tuple. wcoj inner-loop counters (e.g. seeks per
  query) are plain counters incremented and read once at query completion, not timed.

## 6. Export

A new `GET /metrics` route on the axum server (behind `server`), calling
`prometheus_client::encode` over the global registry. Wired in
`crates/sparql/src/server/mod.rs::build_router`. No auth in this phase (operators put it
behind their own network policy / the collector).

## 7. Metric inventory

### 7.1 Slice 1 (first PR — end-to-end vertical slice)

Goal: a live operator dashboard from the first PR, proving registry → handles →
scrape-endpoint → expensive-gauge-collector all work end to end.

**`horndb-metrics` (framework)**
- Global registry + `OnceLock` accessor, `Metrics` struct, label types, the `Collector`
  plumbing for scrape-time gauges.

**sparql HTTP layer** (highest operator value; nothing exists today)
- `horndb_sparql_requests_total{endpoint,method,status}` — counter.
- `horndb_sparql_request_duration_seconds{endpoint}` — histogram (per request).
- `horndb_sparql_query_total{kind}` — counter, kind ∈ {select,ask,construct,describe,update}.
- `horndb_sparql_query_errors_total{stage}` — counter, stage ∈ {parse,translate,plan,exec}.
- Developer histograms: `parse_duration_seconds`, `plan_duration_seconds`,
  `exec_duration_seconds`.
- Integration point: an axum middleware layer for request/latency/status; per-stage
  timing inside `execute_query` / `execute_update`.
- **Deferred to fan-out:** request/response **byte** counters. A middleware can't see the
  serialized response size cheaply; this wants a dedicated body-counting tower layer, so
  it moves to §7.2 rather than shipping as a zero series.

**closure** (`ClosureMetrics` already ~90% there — register as gauges/histograms)
- `horndb_closure_mxm_seconds`, `horndb_closure_total_seconds` — histograms.
- `horndb_closure_iterations_to_fixpoint` — histogram.
- `horndb_closure_input_nnz` / `horndb_closure_output_nnz` — observed per call.

**storage** (`TierStats` / `LoadStats` / `SnapshotStats` exist — scrape-time gauges)
- `horndb_storage_dictionary_terms` — gauge (scrape-time, reads dictionary len).
- `horndb_storage_tier_bytes_estimated` — gauge (scrape-time).
- `horndb_storage_triples` / `graphs` / `predicates` — gauges (scrape-time).
- `horndb_storage_load_duration_seconds`, `horndb_storage_load_bytes_total` — load path.

**`/metrics` endpoint** on the axum server.

### 7.2 Fan-out (follow-on PRs)

- **owlrl** — `rule_fires_total{rule}`, `triples_inferred_total`, `rounds`,
  per-phase duration histograms (already timed in `PhaseTimings`), per-rule latency,
  dirty-predicate prune skip rate. (`Stats`/`PhaseTimings` exist.)
- **incremental** — tick latency histogram, `asserted_merged_total`,
  `derived_merged_total`, closure retract/promote cardinalities, fixpoint rounds,
  change-feed `subscriber_count` gauge. (`TickReport` exists.)
- **ml** — `nl_query_total{result}`, LLM `prompt_tokens` / `completion_tokens` /
  `estimated_usd` (from `CostJson`), translate/execute latency, audit-query latency.
- **wcoj** (developer-facing, careful) — seeks-per-query and iterations-to-match as
  plain counters read at query completion (NOT per-seek timing); peak active iterators;
  ground-pattern pre-check pass rate.
- **sparql request/response bytes** — a body-counting tower layer (deferred from Slice 1).
- **closure `input_nnz`** — observe alongside the existing `output_nnz` per call.

### 7.3 Memory-tier accommodation (ambition)

The schema must *accommodate* HBM / regular-RAM / CXL byte accounting even though the
tiering is not yet built. The `MemTier { Hbm, Dram, Cxl, Unknown }` label vocabulary
(`#[derive(EncodeLabelValue)]`) is **defined in Slice 1** so the intent is recorded.
**Status:** the enum exists but is **not yet attached** to the storage byte gauges
(Slice 1 emits `storage_tier_bytes_estimated` as an unlabelled gauge). Attaching the
`tier` label — defaulting to `Unknown` — lands with the memory-tiering fan-out (§7.2),
at which point adding real tiers is a value change, not a schema change.

## 8. Testing strategy

- Unit: construct a local `Metrics`, exercise handles, assert encoded output contains
  expected series/labels via `prometheus_client::encode`.
- sparql server test (`--features server`): hit an endpoint, then `GET /metrics`, assert
  `horndb_sparql_requests_total` incremented and the latency histogram has samples.
- Scrape-time gauge test: load data, scrape, assert `horndb_storage_triples` reflects the
  loaded count (proves the `Collector` reads live state).
- Overhead guard: a micro-bench (not recorded to BENCHMARKS.md) confirming a resolved
  counter `.inc()` is on the order of a few ns.

## 9. Acceptance criteria

1. `horndb-metrics` crate exists, builds, sits below `storage` in the dep graph, and owns
   the only `prometheus-client` dependency.
2. Typed labels via `#[derive(EncodeLabelSet)]`; no string-keyed metric APIs in any crate.
3. Global `OnceLock` registry with free accessors; hot-path update is a direct atomic op.
4. Slice-1 metrics (§7.1) are live and `GET /metrics` returns valid OpenMetrics text
   behind the `server` feature.
5. Expensive gauges (dictionary/tier sizes) are computed at scrape time via a `Collector`,
   not maintained inline.
6. Histogram instrumentation respects the §5.3 boundary (no per-tuple timing).
7. Tests in §8 pass; `cargo clippy --workspace --all-targets -- -D warnings` is clean.
8. `docs/architecture.md` and `TASKS.md` updated; GitHub tracking issue mirrored.

## 10. Docs sync

- `docs/architecture.md`: add an Observability/Metrics row (Status: implemented for
  Slice 1, planned for fan-out).
- `TASKS.md`: add the metrics epic + slice-1 and fan-out tasks; mirror to a GitHub issue
  per the TASKS.md header procedure.
