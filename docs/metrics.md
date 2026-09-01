# HornDB metrics reference

Authoritative list of every metric and label HornDB exposes. The design rationale
is in [`specs/SPEC-17-metrics.md`](specs/SPEC-17-metrics.md);
this file is the *inventory*. To **diagnose** a performance problem with these
metrics, use the `horndb-perftest-with-metrics` skill — it maps symptoms to the
metrics below.

> **Keep this file in sync with the code.** When you add, remove, or change a metric
> or a label in `crates/metrics/` (or a subsystem's emit site), update the matching
> row here in the **same commit**. The metric definitions in
> `crates/metrics/src/*.rs` are the source of truth; if this file disagrees with
> them, the code wins — fix this file.

## Conventions

- **Registry prefix.** All metrics live under the `horndb` prefix (the registry is
  `Registry::with_prefix("horndb")` in `crates/metrics/src/lib.rs`). Every exposed
  name therefore starts with `horndb_`.
- **OpenMetrics suffixes** (added automatically at scrape time, *not* part of the
  registered name):
  - Counters expose `<name>_total` (e.g. registered `sparql_query` → scraped
    `horndb_sparql_query_total`).
  - Histograms expose `<name>_bucket{le=…}`, `<name>_sum`, `<name>_count`.
  - Gauges expose `<name>` verbatim.
- **Naming.** `horndb_<subsystem>_<name>_<unit>` — durations end in `_seconds`,
  byte counts in `_bytes`, raw counts have no unit suffix.
- **Typed labels.** Label values are Rust enums mapped to lowercase strings (see
  `crates/metrics/src/labels.rs`); there are no free-form string labels except the
  OWL-RL `rule` label, which carries the rule id.
- **Histogram buckets** are exponential. The shorthand `(start ×factor ×count)`
  below means `exponential_buckets(start, factor, count)`.

## Reading metrics

The metrics live in a process-global registry and are exported as OpenMetrics text
at `GET /metrics` on the SPARQL server (behind the `server` feature, on by default):

```bash
# Load some data and serve (standard port 3840; query endpoint is /query, not /sparql)
cargo run -p horndb-sparql --bin serve --release -- --data data.nt --bind 127.0.0.1:3840
# add --materialize to run OWL 2 RL forward-chaining before serving

# Scrape from another shell
curl -s http://127.0.0.1:3840/metrics
```

The storage size gauges (`horndb_storage_triples`, …) are computed **at scrape
time** by a `StorageCollector` the server installs over a `Weak` ref to the live
store, and only appear when the server has registered the collector. In tests,
read the registry directly with `horndb_metrics::encode_metrics()`.

They are cheap in steady state, with one exception worth knowing about. Since
HDB-84 the tier appends a write as a sorted run and merges a partition's runs
on the **first read**, and a scrape is a read: the first scrape after a bulk
load (or any batched write nothing has read yet) pays that merge, once per
affected partition — order of a second on a 10M-triple store, and it emits a
`merge_runs` phase sample. Later scrapes are bounded by the partition count
again.

## Labels

| Label | Values | Used by |
|---|---|---|
| `endpoint` | `query`, `update`, `metrics` | sparql request/byte/duration families |
| `method` | `get`, `post` | `sparql_requests` |
| `status` | HTTP status code (u16, e.g. `200`, `400`) | `sparql_requests` |
| `kind` | `select`, `ask`, `construct`, `describe`, `update` | `sparql_query` |
| `stage` | `parse`, `translate`, `plan`, `exec` | sparql query-errors / stage-duration |
| `phase` | `compiled_rules`, `list_rules`, `closure_backend`, `apply` | `owlrl_phase_duration_seconds` |
| `rule` | OWL-RL rule id (string, e.g. `cax-sco`) | `owlrl_rule_fires`, `owlrl_rule_duration_seconds` |
| `tier` | `dram`, `hbm`, `cxl`, `unknown` | `storage_tier_bytes_estimated` (only `unknown` emitted today — tiering is Stage-3) |
| `result` | `ok`, `error` | `ml_nl_query` |
| `kernel` | `intersect`, `lower_bound`, `merge`, `dedup`, `filter_range`, `filter_indices_eq`, `gather` | `simd_kernel_isa` |
| `isa` | `scalar`, `avx2`, `avx512`, `neon` | `simd_kernel_isa` |
| `source` | `table`, `calibrated`, `static` | `simd_kernel_isa` — which selection path chose this `(kernel, isa)` (known-CPU table / micro-calibration / static widest) |

## SPARQL HTTP + pipeline (`crates/metrics/src/sparql.rs`)

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_sparql_requests_total` | counter | `endpoint`, `method`, `status` | count | HTTP requests |
| `horndb_sparql_request_duration_seconds` | histogram | `endpoint` | s `(1e-4 ×3 ×12)` | per-request wall-clock latency; for HTTP-streamed SELECT responses this measures up to response headers (time to first chunk), not the full body drain |
| `horndb_sparql_request_bytes_total` | counter | `endpoint` | bytes | request body bytes (exact at end-of-stream) |
| `horndb_sparql_response_bytes_total` | counter | `endpoint` | bytes | response body bytes |
| `horndb_sparql_query_total` | counter | `kind` | count | query/update operations by kind |
| `horndb_sparql_query_errors_total` | counter | `stage` | count | pipeline errors by stage; `exec` includes mid-stream errors of HTTP-streamed SELECTs (which abort the response body rather than producing a 4xx/5xx) |
| `horndb_sparql_stage_duration_seconds` | histogram | `stage` | s `(1e-4 ×3 ×12)` | per-stage pipeline latency; for HTTP-streamed SELECTs, `exec` measures plan→first-result-chunk (no duration metric covers the full body drain; delivered bytes are visible in `horndb_sparql_response_bytes_total`), and non-SELECT `/query` requests record one extra `parse` observation from streaming-path routing |

Emitted by `crates/sparql/src/server/` (request middleware, `counting_body.rs`) and
`crates/sparql/src/api.rs` (`timed()`, query-kind classification).

## Storage (`crates/metrics/src/storage.rs`)

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_storage_load_duration_seconds` | histogram | — | s `(1e-3 ×3 ×12)` | RDF load wall-clock (per file, or per batch when `--materialize`) |
| `horndb_storage_load_bytes_total` | counter | — | bytes | bytes read during RDF load |
| `horndb_storage_load_phase_nanoseconds_total` | counter | `phase` | ns | nanoseconds spent in each bulk-load phase |
| `horndb_storage_load_phase_rows_total` | counter | `phase` | count | rows each bulk-load phase handled |
| `horndb_storage_triples` | gauge | — | count | live triples in the store **(scrape-time)** |
| `horndb_storage_graphs` | gauge | — | count | distinct named graphs **(scrape-time)** |
| `horndb_storage_predicates` | gauge | — | count | distinct predicates **(scrape-time)** |
| `horndb_storage_dictionary_terms` | gauge | — | count | interned dictionary terms **(scrape-time)** |
| `horndb_storage_tier_bytes_estimated` | gauge | `tier` | bytes | estimated bytes per memory tier **(scrape-time)** |

`phase` values, in the order a bulk load runs them:

| `phase` | Emitted from | What it covers |
|---|---|---|
| `parse` | `crates/bench-trainmarks/src/main.rs` | tokenising the document **and** materialising the triple batch (`materialize` is the second half) |
| `materialize` | `crates/bench-trainmarks/src/main.rs` | the `Vec<(OxTerm, OxTerm, OxTerm)>` build alone; `parse` minus this is tokenisation |
| `dedupe` | `crates/sparql/src/exec/horn.rs` | interning every term and dropping intra-batch-duplicate triples |
| `dedupe_intern` | `crates/sparql/src/exec/horn.rs` | the `Dictionary::intern_quad` call inside `dedupe` (opt-in, see below) |
| `dedupe_intra` | `crates/sparql/src/exec/horn.rs` | the `intra_batch.insert` probe, plus the `QuadKey` build (opt-in) |
| `dedupe_rest` | `crates/sparql/src/exec/horn.rs` | `entries.push` (opt-in) |
| `dedupe_clock` | `crates/sparql/src/exec/horn.rs` | cost of the instrumentation itself (opt-in) |
| `intern` | `crates/storage/src/store.rs` | `Store::apply_quads` interning terms for storage's own ids. Zero on the bulk-load path since HDB-87: `HornBackend` passes the ids it already resolved (`Store::apply_quad_ids`), so only the term-based write path interns here |
| `group` | `crates/storage/src/memory_tier.rs` | grouping the batch by graph then predicate into per-predicate `(s, o)` sets |
| `copy_forward` | `crates/storage/src/memory_tier.rs` | carrying existing partition rows forward with their visibility stamps. `apply_quad_batch` only — `insert_quad_batch` stopped carrying rows forward in HDB-84 and no longer emits this |
| `merge` | `crates/storage/src/memory_tier.rs` | appending new rows, skipping ones still visible after the deletes (`apply_quad_batch`) |
| `build` | `crates/storage/src/memory_tier.rs` | sorting rows and materialising their Arrow columns: the whole partition in `apply_quad_batch`, only the batch's own new run in `insert_quad_batch` |
| `merge_runs` | `crates/storage/src/partition.rs` | building a partition's readable columns from its sorted runs — the merge sort, plus the object-major sort when the predicate is over `hot_threshold`. This is where `insert_quad_batch`'s former `copy_forward` + whole-partition `build` cost went (HDB-84). Two emission sites: normally the **first read** after batched writes, once per partition; but also from the write itself when a partition hits `MAX_RUNS` runs, and that one nests inside `insert_quad_batch`'s `build` window — **at the cap the same nanoseconds are counted in both phases**, while `build`'s row count still covers only the batch |
| `invalidate` | `crates/sparql/src/exec/horn.rs` | dropping the cached WCOJ snapshots after the write |

The pair is a count+sum summary per SPEC-17 §5.4.1 — mean cost per row for a
phase is `rate(nanoseconds) / rate(rows)`. Each phase accumulates in locals and
touches its counters once per batch, never per row.

The four `dedupe_*` sub-phases are **off by default**; set
`HORNDB_DEDUPE_SUBPHASES=1` to emit them. Splitting a per-triple loop needs a
clock read between each step, which costs the `dedupe` phase 15-25%, so this is
a diagnostic and not the production path. `dedupe_clock` measures one such read
in situ (an empty interval per iteration); every other interval carries the same
cost, so a corrected sub-phase is `dedupe_<x> - dedupe_clock`. With the flag on,
`dedupe` itself reports the inflated total; compare against a run with the flag
off for the real one. `rows` is the batch's input triple count for all four.

## Closure / GraphBLAS (`crates/metrics/src/closure.rs`)

Fed once per closure call (not per iteration) by `crates/closure/src/metrics.rs`.

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_closure_mxm_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | time in `GrB_mxm` per call |
| `horndb_closure_total_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | total closure wall time per call |
| `horndb_closure_iterations_to_fixpoint` | histogram | — | count `(1 ×2 ×10)` | iterations to reach fixpoint |
| `horndb_closure_input_nnz` | histogram | — | count `(10 ×10 ×9)` | input matrix non-zeros |
| `horndb_closure_output_nnz` | histogram | — | count `(10 ×10 ×9)` | output matrix non-zeros |

## OWL 2 RL materialization (`crates/metrics/src/owlrl.rs`)

Emitted by `crates/owlrl/src/engine.rs` — per-rule at the fire site, aggregates once
per `materialize_with` call.

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_owlrl_rule_fires_total` | counter | `rule` | count | fire count per rule id |
| `horndb_owlrl_rule_duration_seconds` | histogram | `rule` | s `(1e-4 ×3 ×12)` | per-rule fire latency |
| `horndb_owlrl_phase_duration_seconds` | histogram | `phase` | s `(1e-4 ×3 ×12)` | per-phase materialize latency |
| `horndb_owlrl_triples_inferred_total` | counter | — | count | triples inferred |
| `horndb_owlrl_rounds_total` | counter | — | count | semi-naïve rounds executed |
| `horndb_owlrl_rule_pruned_total` | counter | — | count | rule evaluations skipped by the dirty-predicate prune |
| `horndb_owlrl_rule_considered_total` | counter | — | count | rule evaluations considered (prune denominator) |

## Incremental maintenance (`crates/metrics/src/incremental.rs`)

Emitted by `crates/incremental/src/circuit.rs` (per tick) and `change_feed.rs`.

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_incremental_tick_duration_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | per-tick circuit latency |
| `horndb_incremental_asserted_merged_total` | counter | — | count | asserted triples merged per tick |
| `horndb_incremental_derived_merged_total` | counter | — | count | derived triples merged per tick |
| `horndb_incremental_closure_withdraw_total` | counter | — | count | closure triples withdrawn on retract |
| `horndb_incremental_closure_promote_total` | counter | — | count | closure triples promoted on retract |
| `horndb_incremental_fixpoint_rounds` | histogram | — | count `(1 ×2 ×10)` | fixpoint rounds per tick |
| `horndb_incremental_distinct_trace_keys` | gauge | — | count | rows in the per-rule weight trace (`rule_weights`), set at the end of every tick |
| `horndb_incremental_change_feed_subscribers` | gauge | — | count | live change-feed subscribers |

## ML / LLM boundary (`crates/metrics/src/ml.rs`)

Emitted by `horndb-ml`'s server module, behind the `server` feature.

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_ml_nl_query_total` | counter | `result` | count | NL queries by success/failure |
| `horndb_ml_prompt_tokens_total` | counter | — | count | LLM prompt tokens consumed |
| `horndb_ml_completion_tokens_total` | counter | — | count | LLM completion tokens produced |
| `horndb_ml_estimated_usd_total` | counter (f64) | — | USD | estimated LLM spend |
| `horndb_ml_translate_duration_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | NL→SPARQL translate latency |
| `horndb_ml_execute_duration_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | translated-query execute latency |
| `horndb_ml_audit_query_duration_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | ML audit-log query latency |

## WCOJ join executor (`crates/metrics/src/wcoj.rs`)

Emitted by `crates/wcoj/src/executor/wcoj.rs` **once per query** (on `BatchIter`
drop) — never per-seek/per-tuple (design §5.3).

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_wcoj_seeks_per_query` | histogram | — | count `(1 ×4 ×12)` | trie-iterator seeks per WCOJ query |
| `horndb_wcoj_iterations_per_query` | histogram | — | count `(1 ×4 ×12)` | leapfrog convergence iterations per query |
| `horndb_wcoj_peak_iterators` | histogram | — | count `(1 ×2 ×12)` | active trie iterators per query |

## SIMD kernel selection (`crates/metrics/src/simd.rs`)

Emitted once at server startup by `crates/sparql/src/bin/serve.rs`
(`record_simd_calibration`), which runs `horndb_simd::init()` and publishes
`horndb_simd::calibration_report()`. One series is set to `1` per primitive — the
`(kernel, isa, source)` chosen by `horndb-simd` startup selection, where `source`
records which path picked it (known-CPU table / micro-calibration / static widest).

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_simd_kernel_isa` | gauge | `kernel`, `isa`, `source` | count | 1 on the `(kernel, isa, source)` series chosen by startup selection; `source` = which path chose it (table/calibrated/static); emitted once at server startup |

> **Caveat.** With auto-tune seeded off (`[simd].autotune = false` /
> `HORNDB_SIMD__AUTOTUNE`) on x86, `merge` (all arms) and
> `filter_range` (its AVX2 arm) report their widest *available* ISA label even
> though those kernels run scalar bodies there; the default autotune-on path
> reports `scalar` for them correctly. (`filter_range`'s NEON arm is genuinely
> vectorized, so this only affects the x86 server where the metric is emitted.)
>
> **Caveat (`intersect`).** The `intersect` series reports the *balanced-input*
> block kernel chosen by the table/calibration. `intersect` additionally applies
> a size-ratio skew-gate at call time: skewed operands (the common leapfrog
> `active_run` shape, e.g. 3 keys vs 50 000) dispatch to the ISA-independent
> scalar gallop, not the reported kernel. So on an unlisted CPU whose calibration
> picks `intersect=avx2`, the join hot path still runs scalar gallop for skewed
> inputs even though the series shows `avx2`. On the two table-pinned hosts
> (Zen4, Sapphire Rapids) `intersect=scalar`, so there is no discrepancy there.
