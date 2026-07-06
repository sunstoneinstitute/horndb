# SPB-256: record the full driver report, not just the 3 headline metrics

**Date:** 2026-06-30
**Status:** design (approved)
**Area:** `crates/harness` (SPEC-01), `nightly.yml`

## Problem

The nightly LDBC SPB-256 job runs the upstream Java driver, which emits a rich
cumulative report (per-query-type latency, editorial per-operation breakdown,
totals, error counts). `crates/harness/src/ldbc_spb.rs::parse_report` scrapes only
three numbers from it — `editorial-qps`, `aggregation-qps`, `duration-s` — and
discards the rest. We want the nightly job to **record every metric the SPB-256
benchmark produces** into the append-only harness trend DB, so per-query
regressions and error spikes are visible in history (not just aggregate QPS).

## What the driver actually prints

Authoritative source: `TestDriverReporter.java` in `ldbc/ldbc_spb_bm_2.0`. The
reporter prints a header **once**, then a cumulative block ~once per second; the
**final** block carries the run's headline averages. Exact format strings:

Header (printed once):
```
\tCreative Works\t: %,d          # thousands-separated
\tReference Entities\t: %,d
\tGeo Locations\t\t: %,d
```

Per block:
```
Seconds : %d
<timestamp> (completed query mixes : %d)      # OR "(completed query runs : %d)"
\tEditorial:
\t\t%s agents
\t\t%-5d inserts (avg : %-7d ms, min : %-7d ms, max : %-7d ms)   # verbose only
\t\t%-5d updates (avg : %-7d ms, min : %-7d ms, max : %-7d ms)   # verbose only
\t\t%-5d deletes (avg : %-7d ms, min : %-7d ms, max : %-7d ms)   # verbose only
\t\t%d operations (%d CW Inserts (%d errors), %d CW Updates (%d errors), %d CW Deletions (%d errors))  # verbose
\t\t%.4f average operations per second
\tAggregation:
\t\t%s agents
\t\t%-5d Q%-2d  queries (avg : %-7d ms, min : %-7d ms, max : %-7d ms, %d errors)   # verbose, one per query type
\t\t%d total retrieval queries (%d errors)
\t\t%.4f average queries per second
```

Non-verbose mode collapses the editorial/aggregation detail to bare counts
(`%d operations (...)`, `%-5d Q%-2d queries`, `%d total retrieval queries`). The
nightly scenario sets `verbose=true`, so the detailed lines are present; the parser
must still tolerate their absence (treat each detail section as optional).

## Approach

Extend the existing parse-and-record path in `crates/harness/src/ldbc_spb.rs`. Both
engine legs (`horndb`, `graphdb-free`) and the uploaded sqlite artifact get the new
metrics for free, because both run scripts call `harness spb-run`, which calls
`ldbc_spb::run → parse_report → record`. **No schema migration** — the `metrics`
table is one row per `(run_id, suite, dataset, metric_name, value, units)`, so new
metric names are purely additive. **No functional change to `nightly.yml`** — it
already invokes the recording path.

### Data shape (`SpbResult`)

Keep the three existing fields (stable). Add:

- `per_query: Vec<QueryStat>` — `QueryStat { id: u32, count: u64, timing: Option<Timing>, errors: Option<u64> }` where `Timing { avg_ms, min_ms, max_ms }`, one per `Q<n>` line, parsed **dynamically** (do not hardcode 12). `timing`/`errors` are `Some` only when the verbose detail line is present; a non-verbose run yields `count` only, so we never fabricate zeros.
- `editorial: EditorialStats` — per-op `OpStat { count: u64, avg_ms, min_ms, max_ms, errors }` (timing+errors `Option`, verbose-only) for inserts/updates/deletes, plus `total_ops: u64`.
- `aggregation_total_queries: u64`, `aggregation_errors: Option<u64>`.
- `dataset: DatasetInfo { creative_works: u64, reference_entities: u64, geo_locations: u64 }` (header, always present).
- `completed: Completed::Mixes(u64) | Runs(u64)` — whichever the block printed.

### Parser

Same "take the **last** occurrence" strategy already in use (robust against the
repeated ~1/sec blocks). Implement with line-oriented regexes (variable whitespace
from `%-5d`/`%-7d` padding makes fixed-offset parsing brittle):

- Per-query lines keyed by `Q<n>` id → last value wins (final cumulative block).
- Dataset-info counts: strip `,` before parsing (`%,d`).
- The three existing headline metrics remain **required** — their absence is the
  "run did not complete" error, exactly as today. Every new section is **optional**:
  if absent, omit those metrics rather than failing.

### Recorder (`record`)

Keep the three existing metric names verbatim (`editorial-qps`, `aggregation-qps`,
`duration-s`) — `harness report --metric <name>` and the README query them by name.
Add, with stable kebab names (units in parens):

| Metric name | units | source |
|---|---|---|
| `q<N>-count` | ops | per-query count |
| `q<N>-avg-ms` / `-min-ms` / `-max-ms` | ms | per-query timing (verbose) |
| `q<N>-errors` | count | per-query errors (verbose) |
| `aggregation-total-queries` | ops | total retrieval queries |
| `aggregation-errors` | count | total retrieval errors (verbose) |
| `editorial-total-ops` | ops | editorial operations total |
| `editorial-{insert,update,delete}-count` | ops | per-op counts (verbose) |
| `editorial-{insert,update,delete}-{avg,min,max}-ms` | ms | per-op timing (verbose) |
| `editorial-{insert,update,delete}-errors` | count | per-op errors (verbose) |
| `cw-count` | count | Creative Works (header) |
| `reference-entities` | count | header |
| `geo-locations` | count | header |
| `completed-query-mixes` **or** `completed-query-runs` | count | whichever printed |

Optional fields are only recorded when present (no fabricated zeros for
non-verbose runs).

### Workflow

`nightly.yml` is unchanged functionally. The `publish benchmark summary` step keeps
rendering the focused `aggregation-qps` A/B trend; the full metric set lands in the
uploaded cumulative sqlite artifact (`harness-<run_id>`). No raw-report artifact
(YAGNI — full DB capture was chosen).

## Testing

- Extend `parse_report` unit tests with a realistic **multi-block, verbose** fixture
  (header + two blocks; assert the parser takes the final block) covering: per-query
  `Q1..Qn` with errors, editorial insert/update/delete timing + the `(... errors)`
  totals line, dataset-info with thousands separators, `completed query mixes`.
- Add a **non-verbose** fixture asserting count-only fields populate and timing/error
  `Option`s are `None` (parser tolerates missing detail).
- Keep `missing_metrics_is_an_error` (headline-required gate).
- Add a `record` test against an in-memory `Db` asserting the expected row count and
  spot-checking a few `(metric_name, dataset, value, units)` rows.

## Docs to sync (same commit as the code)

- `.claude/skills/nightly-benchmarks/SKILL.md` — expand the "Metrics recorded" table.
- `crates/harness/src/ldbc_spb.rs` module doc — replace the "What we parse back out"
  section to describe the full set.
- `BENCHMARKS.md` — note the expanded SPB metric surface on the relevant row.

Out of scope: `docs/metrics.md` (that is the Prometheus `crates/metrics` surface, a
different store from the harness sqlite DB).

## Acceptance criteria

1. After a nightly SPB-256 run, the harness sqlite holds, per engine leg, every
   metric in the table above that the driver printed (verbose run → all of them).
2. The three legacy metric names and their units are unchanged.
3. `parse_report` parses a verbose multi-block report into all fields and a
   non-verbose report into count-only fields without error; missing headline metrics
   still error.
4. `cargo nextest run -p horndb-harness` is green; `cargo clippy -p horndb-harness
   --all-targets -- -D warnings` is clean.
