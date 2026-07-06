---
name: horndb-perftest-with-metrics
description: Diagnose HornDB performance via its Prometheus metrics with a symptom→metric decision tree. Use when a HornDB query, materialization, or data load is slow or regressed, when investigating latency/throughput, or when asked which metric explains a performance issue.
---

# Diagnosing HornDB performance with metrics

HornDB exposes typed Prometheus metrics under the `horndb_` prefix at `GET /metrics`.
This skill turns a performance *symptom* into the right metrics to look at. The full
inventory (every series, label, unit, bucket) is [`docs/metrics.md`](../../../docs/metrics.md)
— consult it for exact names; this skill is the diagnostic playbook.

## 1. Get metrics in front of you

```bash
# Serve some data (standard port 3840; query endpoint is /query, NOT /sparql).
# Add --materialize to run OWL 2 RL forward-chaining before serving.
cargo run -p horndb-sparql --bin serve --release -- --data data.nt --bind 127.0.0.1:3840

# Drive the workload you want to profile (curl /query, the harness, a bench), then scrape:
curl -s http://127.0.0.1:3840/metrics > /tmp/m.txt
grep '^horndb_' /tmp/m.txt
```

For a trend over time, point a Prometheus (or an OTel Collector's Prometheus receiver)
at `http://<bind>/metrics`. In tests, read the registry directly with
`horndb_metrics::encode_metrics()`.

## 2. Read a histogram

Latency/cardinality metrics are histograms: each exposes `_bucket{le=…}`, `_sum`, `_count`.

- **Quick mean:** `_sum / _count`.
- **Eyeball the tail** from the raw text: find the `le` bucket where the cumulative
  count crosses 0.5 / 0.95 / 0.99 of `_count` — that's your P50 / P95 / P99.
- **In PromQL:** `histogram_quantile(0.99, sum by (le) (rate(horndb_sparql_request_duration_seconds_bucket[5m])))`.

Counters expose `<name>_total`; rates of change matter more than absolute values
(`rate(horndb_owlrl_rule_fires_total[5m])`). Gauges are point-in-time.

## 3. Symptom → metric decision tree

Work top-down: start at the request, then drill into the stage the time is in.

**Query is slow / latency spiked**
1. `horndb_sparql_request_duration_seconds{endpoint="query"}` — confirm and quantify (P50/P95/P99).
2. Localize the stage: `horndb_sparql_stage_duration_seconds{stage=…}` — `parse` / `translate` / `plan` / `exec`. Whichever dominates is your culprit.
3. If `exec` dominates → jump to **Join is slow**. If `plan` dominates → planner/cardinality estimation. Check `horndb_sparql_query_errors_total{stage=…}` for failures masquerading as latency.
4. Big result sets: compare `horndb_sparql_response_bytes_total{endpoint="query"}` against request rate — serialization volume, not join cost.

**Join is slow (WCOJ)** — `horndb_wcoj_*`, observed once per query
- `seeks_per_query` ≫ `iterations_per_query` → poor selectivity / data skew forcing many seeks per leapfrog step.
- High `iterations_per_query` → expensive convergence; revisit join order / variable ordering.
- High `peak_iterators` → wide BGP (many patterns active at once).
- *Blind spot:* there is **no per-seek/per-tuple timing** (design §5.3). For nanosecond-level join cost use the `per_tuple` criterion bench, not metrics.

**Materialization (OWL 2 RL) is slow** — `horndb_owlrl_*`
1. `phase_duration_seconds{phase=…}` — which phase? `apply` → rule application or delta merge; `closure_backend` → hand off to **Closure**; `compiled_rules` / `list_rules` → setup.
2. `rule_duration_seconds{rule=…}` + `rule_fires_total{rule=…}` — find the heavy-hitter rule id (e.g. `cax-sco`).
3. `rounds_total` — too many semi-naïve rounds → fixpoint is expensive.
4. Prune effectiveness: `rule_pruned_total / rule_considered_total`. Near 1.0 = the dirty-predicate prune is skipping most work (good); low ratio = prune is ineffective and rules are re-evaluated needlessly.
5. Sanity-check `triples_inferred_total` against expectation — undercount hints the prune is too aggressive; runaway count hints a rule feedback loop.

**Closure doesn't converge / explodes** — `horndb_closure_*`, once per call
- `iterations_to_fixpoint` spiking → hard reachability / data pathology.
- `total_seconds` ≫ `mxm_seconds` → overhead is in setup/conversion, not GraphBLAS itself; if they track, it's genuine `GrB_mxm` cost.
- `output_nnz` ≫ `input_nnz` (e.g. >100×) → closure explosion; suspect a transitive predicate or feedback.

**Incremental update lags** — `horndb_incremental_*`, per tick
- `tick_duration_seconds` rising over time → degrading maintenance.
- `fixpoint_rounds` spiking → a delta triggers a deep re-derivation (feedback or pathological data).
- Retraction cost: `closure_withdraw_total` / `closure_promote_total` growth.
- `change_feed_subscribers` > 0 and ticks slow when it is → a slow subscriber is applying backpressure.

**Data load is slow** — `horndb_storage_*`
- `load_duration_seconds` (P99 for an outlier file) and `load_bytes_total` → derive bytes/sec.
- Watch `--materialize`: that path records the whole parse+materialize+load span as **one** observation, not per file.

**Store / memory growth** — scrape-time gauges (cost nothing in steady state)
- `storage_triples`, `storage_graphs`, `storage_predicates` over time.
- `storage_dictionary_terms` vs `storage_triples`: a ratio climbing past ~1.0 hints term explosion / a dictionary bug.
- `storage_tier_bytes_estimated{tier=…}`: **only `tier="unknown"` is emitted today** — HBM/CXL tiering is Stage-3, so don't read tier breakdown into it yet.

**LLM cost / NL-query spend** — `horndb_ml_*` (server feature)
- `estimated_usd_total`, `prompt_tokens_total`, `completion_tokens_total` — watch the rate, not just the total.
- `nl_query_total{result="error"}` rate → failing translations burning tokens.
- `translate_duration_seconds` vs `execute_duration_seconds` — is the LLM round-trip or the SPARQL execution the latency?

## 4. Gotchas

- **Storage size gauges are scrape-time only** and appear only when the `serve`
  binary has installed the `StorageCollector`. A bare in-process test won't show them.
- **No per-tuple / per-seek timing anywhere** — that boundary is deliberate (design
  §5.3). For sub-microsecond costs, reach for the criterion benches (`per_tuple`,
  `four_cycle`, `load_lubm`, …) on `hornbench`, not metrics.
- **Metrics ≠ recorded benchmarks.** `/metrics` is live in-process observation;
  `docs/benchmarks.md` numbers come from `cargo bench` on `hornbench`. Don't record a
  scraped number as a benchmark result.
- ML metrics are behind the `server` feature; the `/metrics` endpoint itself is
  behind `server` (on by default in `horndb-sparql`).

See [`docs/metrics.md`](../../../docs/metrics.md) for the authoritative series list and
[`docs/specs/2026-06-29-metrics-design.md`](../../../docs/specs/2026-06-29-metrics-design.md) for design rationale.
