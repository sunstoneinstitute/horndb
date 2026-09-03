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
`merge_runs` phase sample plus a `storage_partition_merge_seconds` observation
with `trigger="read"`. Later scrapes are bounded by the partition count again.
Since HDB-122 the merge runs outside the partition's `runs` mutex, so it no
longer blocks writers to that partition while it is in flight.

## Labels

| Label | Values | Used by |
|---|---|---|
| `endpoint` | `query`, `update`, `metrics` | sparql request/byte/duration families |
| `method` | `get`, `post` | `sparql_requests` |
| `status` | HTTP status code (u16, e.g. `200`, `400`) | `sparql_requests` |
| `kind` | `select`, `ask`, `construct`, `describe`, `update` | `sparql_query` |
| `stage` | `parse`, `translate`, `plan`, `exec` | sparql query-errors / stage-duration |
| `phase` | `compiled_rules`, `list_rules`, `closure_backend`, `apply` | `owlrl_phase_duration_seconds` |
| `backend` | `rule-firing`, `graphblas` | `reasoning_backend` — matches the `[reasoning].backend` config value verbatim |
| `rule` | OWL-RL rule id (string, e.g. `cax-sco`) | `owlrl_rule_fires`, `owlrl_rule_duration_seconds` |
| `tier` | `dram`, `hbm`, `cxl`, `unknown` | `storage_tier_bytes_estimated` (only `unknown` emitted today — tiering is Stage-3) |
| `result` | `ok`, `error` | `ml_nl_query` |
| `kernel` | `intersect`, `lower_bound`, `merge`, `dedup`, `filter_range`, `filter_indices_eq`, `gather` | `simd_kernel_isa` |
| `isa` | `scalar`, `avx2`, `avx512`, `neon` | `simd_kernel_isa` |
| `trigger` | `read`, `write_cap` | `storage_partition_merges` — what made a partition merge its runs |
| `source` | `table`, `calibrated`, `static` | `simd_kernel_isa` — which selection path chose this `(kernel, isa)` (known-CPU table / micro-calibration / static widest) |

## SPARQL HTTP + pipeline (`crates/metrics/src/sparql.rs`)

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_sparql_requests_total` | counter | `endpoint`, `method`, `status` | count | HTTP requests |
| `horndb_sparql_request_duration_seconds` | histogram | `endpoint` | s `(1e-4 ×3 ×12)` | per-request wall-clock latency; for HTTP-streamed SELECT responses this measures up to response headers (time to first chunk), not the full body drain |
| `horndb_sparql_request_bytes_total` | counter | `endpoint` | bytes | request body bytes (exact at end-of-stream) |
| `horndb_sparql_response_bytes_total` | counter | `endpoint` | bytes | response body bytes |
| `horndb_sparql_query_total` | counter | `kind` | count | query/update operations by kind |
| `horndb_sparql_query_errors_total` | counter | `stage` | count | pipeline errors by stage; `exec` includes mid-stream errors of HTTP-streamed SELECTs, serializer panics included (both abort the response body rather than producing a 4xx/5xx) |
| `horndb_sparql_stage_duration_seconds` | histogram | `stage` | s `(1e-4 ×3 ×12)` | per-stage pipeline latency; for HTTP-streamed SELECTs, `exec` measures plan→first-result-chunk (no duration metric covers the full body drain; delivered bytes are visible in `horndb_sparql_response_bytes_total`), and non-SELECT `/query` requests record one extra `parse` observation from streaming-path routing |
| `horndb_sparql_exec_phase_nanoseconds_total` | counter | `phase` | ns | nanoseconds spent in each per-operator SPARQL execution-time phase (`HORNDB_EXEC_PHASES=1` only — zero rows/samples when the flag is off) |
| `horndb_sparql_exec_phase_rows_total` | counter | `phase` | count | rows each execution-time phase handled |
| `horndb_sparql_queries_in_flight` | gauge | none | count | `/query` requests currently holding an admission-control permit (HDB-118). A streamed SELECT holds its permit until the client finishes draining the body, so this tracks live executions, not just plan+first-chunk. Ceiling: `[server.limits].max_concurrent_queries`. |
| `horndb_sparql_queries_rejected_total` | counter | none | count | `/query` requests shed with HTTP 503 + `Retry-After` because no permit came free within `[server.limits].queue_timeout` (HDB-118). Oversized bodies are *not* counted here — they are refused by the body-size layer and appear as `horndb_sparql_requests_total{status="413"}`. |
| `horndb_sparql_stats_rebuild_total` | counter | none | count | full rebuilds of the planner's `SnapshotStats` summary (an `O(store)` scan). A write merges its quad delta into the cached summary instead, so a healthy write-then-read feed keeps this flat; it moves only when a scope is first planned, when a write cannot be expressed as a delta, or when accumulated drift passes `STATS_DRIFT_DIVISOR` (1/10 of the rows) |
| `horndb_sparql_stats_rebuild_seconds` | histogram | none | s `(1e-4 ×3 ×12)` | latency of those full rebuilds |

Emitted by `crates/sparql/src/server/` (request middleware, `counting_body.rs`,
admission control in `mod.rs`),
`crates/sparql/src/api.rs` (`timed()`, query-kind classification),
`crates/sparql/src/exec/horn.rs` (`snapshot_stats`, the planner-statistics
cache), and `crates/sparql/src/exec/phases.rs` (the exec-phase split, below).

### SPARQL execution-time phases (`crates/sparql/src/exec/phases.rs`)

Off by default; set `HORNDB_EXEC_PHASES=1` to emit `horndb_sparql_exec_phase_*`.
Splits the single `exec` pipeline stage
(`horndb_sparql_stage_duration_seconds{stage="exec"}`) into the operators that
actually spend the time, so a slow query can be attributed to a cause instead of
leaving everything inside one `exec` number (HDB-99). The gate is a `OnceLock`
read, checked at batch/chunk/group granularity — never per row (SPEC-17 §5.3) —
so it costs nothing measurable when off.

| `phase` | Emitted from | What it covers |
|---|---|---|
| `scan_wcoj` | `crates/sparql/src/exec/horn.rs` (`scan_bgp_ids`) | the WCOJ (leapfrog triejoin) executor producing one arrow batch of join results |
| `scan_row_build` | `crates/sparql/src/exec/horn.rs` (`scan_bgp_ids`) | converting one arrow batch's columns into slot `Row`s, including the diagonal (repeated-variable) filter — one pair per arrow batch, not per row |
| `scan_provenance` | `crates/sparql/src/exec/op/source.rs` (`ScanOp::new`) | the O(rows × cols) walk that decides which output columns may carry a decoded `Term` |
| `join_build` | `crates/sparql/src/exec/op/blocking.rs` (`JoinOp`/`LeftJoinOp`, `build_join_state`) | indexing a hash join's build (right) side by join key. Fires only for a query with an actual `Join`/`LeftJoin` node — a single flat BGP (trainmarks q2/q3) folds into one `BgpScan` and has neither |
| `join_probe` | `crates/sparql/src/exec/op/blocking.rs` (`probe_join_chunk`/`probe_left_join_chunk`) | probing one chunk of the streamed (left) side against the build index |
| `group_key` | `crates/sparql/src/exec/runtime.rs` (`eval_group_native`) | hashing each input row into its `GROUP BY` bucket |
| `group_decode` | `crates/sparql/src/exec/runtime.rs` (`eval_group_native`) | per group: decoding the sort key and the columns the aggregates read (`decode_subset`) |
| `agg_fold` | `crates/sparql/src/exec/runtime.rs` (`eval_group_native`) | per group: folding the aggregate functions over the decoded members |
| `sort` | `crates/sparql/src/exec/runtime.rs` (`compute_order_by`; the group-output sort in `eval_group_native`) | `ORDER BY`'s decorate-and-sort, and the lexical sort `GROUP BY` applies to its output groups |
| `stream_op` | `crates/sparql/src/exec/op/stream.rs` (`Extend`/`Project`/`Filter`/`Distinct`) | the per-chunk transform itself (BIND, projection, FILTER, DISTINCT dedup) — never the child pull that feeds it |
| `result_encode` | `crates/sparql/src/exec/runtime.rs` (`BindingsStream::next_chunk`, via `Batch::to_bindings`) | decoding one operator chunk's slot rows into the `Bindings` the caller sees |
| `clock` | `crates/sparql/src/exec/runtime.rs` (`eval_group_native`) | one empty `Instant::now()` interval per group (HDB-90 style): the cost of the instrumentation itself, to subtract from `group_decode`/`agg_fold` |
| `chunk_pull` | `crates/sparql/src/exec/op/mod.rs` (`ChunkedBatch::next_chunk`) | handing a materialized `Batch` out one `batch_rows()`-sized chunk at a time: the `collect` that moves the rows plus the per-chunk `schema.clone()`. Times the clone's *allocation* only — the matching free happens later, when the chunk is dropped (typically inside `drain`), so it lands in `residual`, not here. One clock per chunk (HDB-109) |
| `drain_extend` | `crates/sparql/src/exec/op/blocking.rs` (`drain`) | concatenating a child's chunks into the one `Batch` a blocking operator (`Group`, `OrderBy`, `Union`, a hash join's build side, `PathClosure`) consumes: the row moves and the accumulating `Vec`'s growth reallocations. Never the child's `next()` (HDB-109) |
| `row_drop` | `crates/sparql/src/exec/runtime.rs` (`eval_group_native`) | per group: freeing that group's member rows and their decoded `Bindings` once the aggregates are folded — one heap free per `Row`'s slot `Vec`. One clock per group (HDB-109) |
| `residual` | `crates/sparql/src/exec/phases.rs` (`flush`) | `exec_elapsed − sum(the other 15 phases)` — everything they don't clock (e.g. the pushdown rewrite in `runtime.rs`, allocation inside untimed operator plumbing, and the drop of any row batch not covered by `row_drop`) |

`decode_subset` (`exec/runtime.rs`) is shared by several call sites
(`compute_order_by`, `compute_path_closure`, `eval_group_native`,
`probe_into_indexed`); it is not its own phase — its cost lands inside
whichever phase's timed span encloses the call (`sort`, `group_decode`,
`join_probe`, …).

Flushed once per query: on the synchronous path, in `api::timed` when
`stage == Stage::Exec`; on the HTTP streaming path, in
`server::query::record_exec`. The streaming server path only measures up to
the first result chunk — same as `horndb_sparql_stage_duration_seconds` already
does for that path (see above) — so its phase split covers "get the first
chunk out", not the full result-set drain.

The pair is a count+sum summary per SPEC-17 §5.4.1, the same convention as the
storage load phases above: mean cost per row is `rate(nanoseconds) /
rate(rows)`. Each phase accumulates in a **thread-local**, not a `Runtime`
field — `HornBackend::scan_bgp_ids` is an `&self` method, and callers may
share one `HornBackend` across query threads behind an `Arc` (e.g.
`bench-trainmarks`) — and touches the shared counters once per query, on flush.

## Storage (`crates/metrics/src/storage.rs`)

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_storage_load_duration_seconds` | histogram | — | s `(1e-3 ×3 ×12)` | RDF load wall-clock (per file, or per batch when `--materialize`) |
| `horndb_storage_load_bytes_total` | counter | — | bytes | bytes read during RDF load |
| `horndb_storage_load_phase_nanoseconds_total` | counter | `phase` | ns | nanoseconds spent in each bulk-load phase |
| `horndb_storage_load_phase_rows_total` | counter | `phase` | count | rows each bulk-load phase handled |
| `horndb_storage_partition_merge_seconds` | histogram | — | s `(1e-4 ×4 ×12)` | wall-clock of **one** partition run merge (HDB-122). `load_phase_nanoseconds{phase="merge_runs"}` sums the same work; this is the distribution, so a p99 of seconds is visible instead of averaged away |
| `horndb_storage_partition_merges_total` | counter | `trigger` | count | partition run merges, by what triggered them. `trigger="read"` is the first read after batched writes — the merge is charged to whichever reader gets there first, including a `/metrics` scrape; `trigger="write_cap"` is a write that hit `MAX_RUNS` and merged rather than appending |
| `horndb_storage_triples` | gauge | — | count | live triples in the store **(scrape-time)** |
| `horndb_storage_graphs` | gauge | — | count | distinct named graphs **(scrape-time)** |
| `horndb_storage_predicates` | gauge | — | count | distinct predicates **(scrape-time)** |
| `horndb_storage_dictionary_terms` | gauge | — | count | dictionary index space consumed — every id ever handed out, monotonic because ids are never re-issued **(scrape-time)** |
| `horndb_storage_dictionary_terms_live` | gauge | — | count | dictionary terms still resolvable, i.e. total minus the slots `Store::compact()`'s sweep reclaimed (HDB-121). Under append + retract churn the total keeps climbing while this one plateaus; the gap is reclaimed lexical bytes, not a leak **(scrape-time)** |
| `horndb_storage_tier_bytes_estimated` | gauge | `tier` | bytes | estimated bytes per memory tier **(scrape-time)** |

`phase` values, in the order a bulk load runs them:

| `phase` | Emitted from | What it covers |
|---|---|---|
| `parse` | `crates/bench-trainmarks/src/main.rs`; `crates/storage/src/loader/{turtle,ntriples,nquads}.rs` | Two emission sites, and they measure different things. From the bench driver: tokenising the document **and** materialising the triple batch (`materialize` is the second half). From the slice loaders (`load_*_slice`): the calling thread's wall clock minus the time it spent interning and inserting, taken once per 8,192-item batch — at one parse thread that is the inline parse, above one it is what the consumer still waits for. **Since HDB-106 that wait also covers the parse threads' dictionary probe**, not just tokenising: from 4 chunks up (`parallel::should_probe`) the parse threads look each term up read-only before sending the batch, so work that used to show up in `intern` now shows up here. Below 4 chunks the probe is gated off and `parse` means what it did before HDB-106. It is a net win — on trainmarks xlarge Turtle at 8 threads `parse` went 0.880s -> 1.299s while `intern` went 3.002s -> 2.136s — but the phase no longer means "tokenising" on that path. A process that does both adds them together |
| `materialize` | `crates/bench-trainmarks/src/main.rs` | the `Vec<(OxTerm, OxTerm, OxTerm)>` build alone; `parse` minus this is tokenisation |
| `dedupe` | `crates/sparql/src/exec/horn.rs` | interning every term of a bulk-insert batch (HDB-104: no longer also drops intra-batch-duplicate triples — `MemoryTier::apply_quad_batch` dedups the add side itself). Also emitted by `HornBackend::load_id_closure`, the reasoning path, where it covers interning the reasoner's dictionary once and remapping the closure ids (HDB-117); `rows` there counts closure triples, not interned terms |
| `intern` | `crates/storage/src/store.rs`; `crates/storage/src/loader/mod.rs` | dictionary interning on the two **term-based** write paths: `Store::apply_quads`, and the bulk loaders' `QuadSink`. Before HDB-106 the loaders emitted nothing here and `intern` had to be read off a load as a residue (wall clock minus the counted phases) — it was 56–61% of a trainmarks xlarge load and the only large phase that was not a counter. `QuadSink` takes one clock pair per 8,192-item parse batch and subtracts the tier flushes that fell inside it, so this is interning (plus the id-tuple push), not interning plus the tier. Clocking it at that granularity is why the **streaming** loaders now buffer up to 8,192 parsed rows before each intern batch (`load_quads_in_graphs`) — bounded, owned terms moved not cloned, and added for the clock, not for any document-buffering reason. Still zero on the `HornBackend` bulk path since HDB-87: it passes the ids it already resolved (`Store::apply_quad_ids`), which interns nothing |
| `intern_encode`, `intern_probe`, `intern_miss`, `intern_reverse` | `crates/storage/src/dictionary.rs` | sub-phase split of `intern`, **emitted only when `HORNDB_INTERN_PHASES=1`** (HDB-106). `intern_encode` is building the byte key, `intern_probe` the forward-map lookup that answers a hit, `intern_miss` the whole slow path a failed probe falls into, and `intern_reverse` the reverse-vector write **inside** `intern_miss`. They **nest** — `encode + probe` is the hit path, and `reverse ⊂ miss` — so the four do not sum to `intern`. Off by default because interning a term costs ~100 ns and the split adds two `Instant::now()` pairs per call: a diagnostic you turn on for one run, like `HORNDB_EXEC_PHASES=1`. The rows counter is **intern calls**, not triples. Accumulated thread-locally and published once per load by `Dictionary::flush_intern_phases` |
| `group` | `crates/storage/src/memory_tier.rs` | grouping the batch by graph then predicate into per-predicate `(s, o)` lists. Since HDB-88 the two sides differ: the del side is a `HashSet` (probed once per existing live row), the add side a `Vec` that this phase then **sorts and deduplicates**. That sort is why the phase's cost depends on the order the corpus arrives in — near-free on a subject-ordered document, a real `n log n` on a shuffled one (in which case `build` skips one instead) |
| `copy_forward` | `crates/storage/src/memory_tier.rs` | carrying existing partition rows forward with their visibility stamps, end-stamping the ones this batch retracts, and collecting the survivors into the sorted `still_visible` list the `merge` phase reads. `apply_quad_batch` only, and since HDB-102 only for the **predicates a batch actually deletes from** — a predicate the batch only adds to takes the append-run path and carries nothing, so an add-only batch (every `INSERT DATA`, every `Store::insert_quads`) contributes zero seconds and zero rows here. `insert_quad_batch` stopped carrying rows forward in HDB-84 and never emits this |
| `merge` | `crates/storage/src/memory_tier.rs` | deciding which of a batch's added pairs are genuinely new, and staging those (`apply_quad_batch`). Both of that call's paths report here, and they scale differently. On the **rebuild** path (predicates this batch deletes from) it is a linear merge cursor over the sorted `still_visible` list, `O(live rows + adds)` per predicate — it grows with the *partition*, not the batch, and went 0.017s to 0.072s on a 16-call 1M append when HDB-88 introduced the cursor (`docs/benchmarks.md`). On the **append-run** path (HDB-102, predicates this batch only adds to) it is `PredicatePartition::mark_live`: one galloping search per run per added pair, `O(adds · log(rows/adds))` per run and **no** pass over the partition. Since HDB-102 an add-only batch's whole read-side cost sits in this phase rather than in `copy_forward` |
| `build` | `crates/storage/src/memory_tier.rs` | sorting rows and materialising their Arrow columns. How much it covers depends on the path: the **whole partition** for an `apply_quad_batch` predicate that is being rebuilt (it has deletes), and only the **batch's own new run** for `insert_quad_batch` and for an `apply_quad_batch` predicate on the append-run path (HDB-102). Since HDB-88 the sort is **skipped when the rows already arrive in `(s, o, begin)` order** — which is the case for a bulk insert into an empty partition, since `group` sorted them — leaving only the dedupe scan and the column materialisation |
| `merge_runs` | `crates/storage/src/partition.rs` | building a partition's readable columns from its sorted runs — the merge sort, plus the object-major sort when the predicate is over `hot_threshold`. This is where `insert_quad_batch`'s former `copy_forward` + whole-partition `build` cost went (HDB-84). Two emission sites: normally the **first read** after batched writes, once per partition; but also from the write itself when a partition hits `MAX_RUNS` runs, and that one nests inside `insert_quad_batch`'s `build` window — **at the cap the same nanoseconds are counted in both phases**, while `build`'s row count still covers only the batch. The two sites are told apart by `storage_partition_merges{trigger}` |
| `invalidate` | `crates/sparql/src/exec/horn.rs` | dropping the cached WCOJ snapshots after the write |

The pair is a count+sum summary per SPEC-17 §5.4.1 — mean cost per row for a
phase is `rate(nanoseconds) / rate(rows)`. Each phase accumulates in locals and
touches its counters once per batch, never per row.

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
| `horndb_reasoning_inconsistent` | gauge | — | 0/1 | 1 iff the materialized closure is OWL 2 RL inconsistent (some individual inferred to be `owl:Nothing`). Set once by `serve --materialize` (HDB-125). Registered in `crates/metrics/src/owlrl.rs` but deliberately **not** under the `owlrl_` prefix — it describes the served closure, not the rule engine. |
| `horndb_reasoning_backend` | gauge | `backend` | count | info gauge: 1 on the closure backend `serve --materialize` used, from `[reasoning].backend`; emitted once at startup |

## Named-graph reasoning views (`crates/metrics/src/reasoning.rs`)

Emitted by `crates/sparql/src/reasoning/` (SPEC-29 P1), behind the `reasoner`
feature and only when `[reasoning].enabled` is set: once per view derivation,
plus two gauges the view manager republishes when the dirty set or the spine
version changes. No labels — P1 serialises derivations, and per-view staleness
is readable as quads from the view catalog graph
(`https://horndb.io/graph/views`) rather than as a high-cardinality series.

| Metric (scraped name) | Type | Labels | Unit / buckets | Meaning |
|---|---|---|---|---|
| `horndb_reasoning_view_derivations_total` | counter | — | count | view derivations completed (SPEC-29 acceptance 6 reads this to prove one-graph updates do work in exactly one view) |
| `horndb_reasoning_views_dirty` | gauge | — | count | views marked stale and awaiting re-derivation |
| `horndb_reasoning_spine_version` | gauge | — | count | current vocabulary-spine version, bumped on any write to a spine graph |
| `horndb_reasoning_derivation_duration_seconds` | histogram | — | s `(1e-4 ×3 ×12)` | wall-clock of one view derivation (fork + extend + diff + apply) |
| `horndb_reasoning_spine_build_duration_seconds` | histogram | — | s `(1e-3 ×3 ×12)` | wall-clock of one spine template build, the closure computed once per spine version and shared by every view (SPEC-29 D3) |

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
