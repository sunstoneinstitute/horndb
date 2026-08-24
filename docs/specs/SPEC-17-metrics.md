---
status: specified
date: 2026-06-29
scope: "Metrics & Observability — Design (Phase 1: Metrics)"
---

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
- No per-tuple / per-`seek()` **timing histograms** (see §5.3 — the timing-histogram
  cost boundary). Tight loops are measured with count+sum counters instead (§5.4).

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

### 5.3 The timing-histogram cost boundary

A timing histogram costs an `Instant::now()` (~20 ns `clock_gettime` through the vDSO)
plus the `observe` itself. **In the library we use, `observe` is not a single atomic.**
`prometheus_client::Histogram` is an `Arc<Mutex<Inner>>` — a `parking_lot` mutex around
a `Vec<(f64, u64)>` of buckets that `observe` walks linearly. One observation runs well
over 100 ns uncontended, and it serializes across threads.

**This is a property of `prometheus_client`, not of histograms.** A histogram observe can
be made to run in ~50 ns: integer (not `f64`) bucket bounds, a binary search rather than
a linear walk, two relaxed atomic `fetch_add`s rather than a mutex, and the sum kept on
its own cache line. `github.com/stigsb/prometheus-cpp` — already cited in §3 for its
typed-label philosophy — implements exactly that, and §5.4 uses its local-accumulator
design. So the boundary below is a limit of the current dependency, and replacing that
`Histogram` is a legitimate way to move it rather than work around it.

That is fine **around a whole query, a fixpoint round, a closure call, or an HTTP
request**. It is **far too expensive per-tuple** in the leapfrog inner loop, where the
instrument would cost more than the work it measures. The design draws the line
explicitly:

- **Yes:** per-query, per-update-tick, per-fixpoint-round, per-closure-call,
  per-HTTP-request, per-load.
- **No:** per-`seek()`, per-`next()`, per-tuple. wcoj inner-loop counters (e.g. seeks per
  query) are plain counters incremented and read once at query completion, not timed.

This rules out **touching a shared histogram** inside a tight loop. It rules out neither
measuring those loops nor keeping their distribution — see §5.4.

### 5.4 Local accumulation, merged once — measuring inside tight loops

The rule for any loop below the §5.3 boundary is the same regardless of instrument:
**touch no shared metric handle while the loop runs.** Accumulate in local state, merge
once on the way out. Two shapes, picked by whether the distribution is worth keeping.

**Take the clock outside the loop either way.** One `Instant::now()` before and one
after. The ~20 ns read amortizes to nothing across the iterations; never call it per
iteration.

#### 5.4.1 Count + sum, when the mean is enough

A pair of counters — `..._total` for operations or bytes, `..._nanoseconds_total` for
elapsed time. Prometheus recovers the mean as `rate(sum) / rate(count)`. In the loop this
is a `u64 +=` on a local: a register add, no atomic, no cache traffic.

This is the right instrument where per-iteration cost is close to uniform, which is the
normal case here — HornDB has no garbage collector to inject pauses and processes
columnar data with regular per-tuple work. What it gives up is the shape: no P99, no
visible bimodality.

#### 5.4.2 Local bucket array, when the shape matters

Keeping the distribution does **not** require paying a histogram's cost in the loop.
Mirror `LocalHistogram` from `stigsb/prometheus-cpp`: hold a plain, non-atomic bucket
array plus a sum alongside the real histogram, binary-search the bounds and bump a local
`counts[idx]` per observation, then merge into the shared histogram once at the end. The
merge skips empty buckets, so it costs one atomic per *occupied* bucket plus one for the
sum — not one per observation.

The loop pays a binary search and two local increments. The distribution survives intact.

Prefer this over 5.4.1 wherever a loop is genuinely spiky and the spike is the thing worth
seeing: allocation-bound sinks, or partition builds crossing `DEFAULT_HOT_THRESHOLD`,
where the eager/lazy object-major decision makes per-partition cost bimodal by
construction. A mean hides exactly the effect you are looking for there.

A Rust equivalent needs integer bounds (nanoseconds are integers — comparing them as
`f64` buys nothing and costs conversions), `partition_point` for the search, a
`[u64; N]` local, and `[AtomicU64; N]` in the shared histogram. None of that exists in
`prometheus_client` today; building it is the §5.3 escape route.

#### 5.4.3 Flush cadence

- **Bounded, known iteration count** — merge once after the loop. A loop over one
  partition, one batch, or one chunk touches its metrics exactly once, on the way out.
- **Unbounded stream** — merge every 100–1000 iterations so a long-running loop still
  reports progress between scrapes. The tighter the body, the larger the interval.

### 5.5 Concurrent accumulators must not share a cache line

This applies the moment a parallel loop flushes into counters — for example the chunked
bulk loaders, where several parse threads run at once.

A `prometheus_client::Counter` is an `Arc<AtomicU64>`. Each counter is its own heap
allocation and `AtomicU64` is 8-byte aligned, so **no single counter can straddle two
64-byte L1 cache lines**. That failure mode does not exist with these types.

The hazard that does exist is **false sharing**: two independently allocated counters can
land in the same 64-byte line, and two threads incrementing them then bounce that line
between cores. The counts stay correct, but each increment pays a cache-coherence round
trip — tens to hundreds of cycles instead of a few. Rules:

- **Flush once per thread, at the end of its work**, whenever the thread count is bounded
  and known. One atomic add per thread cannot contend meaningfully, whatever the layout.
- **If a parallel loop must flush repeatedly**, give each thread its own accumulator
  padded to a full cache line (a `#[repr(align(64))]` wrapper around the value), sum them
  once at the end, and touch the shared counter once. Never let N threads `fetch_add` the
  same counter from inside a loop.
- **Do not assume you control placement.** With `Arc<AtomicU64>` the allocator decides
  where counters land, so two "independent" hot counters may share a line. Padding is
  only available to you on accumulators you own — which is another reason to keep the
  hot path on local or per-thread state and touch the registry once.
- **Inside a metric, separate the contended fields.** `stigsb/prometheus-cpp` puts its
  histogram's `sum_` on its own cache line (`alignas(cache_line_size)`) so the running
  sum does not share a line with the bucket array every observation also touches. Any
  replacement histogram we build should do the same.

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
- Overhead guard: a micro-bench (not recorded to docs/benchmarks.md) confirming a resolved
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
