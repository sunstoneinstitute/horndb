---
name: nightly-benchmarks
description: Work with HornDB's nightly LDBC SPB-256 benchmark — find runs, grab the cumulative sqlite artifact, and query the trend series (editorial-qps, aggregation-qps, duration-s) for HornDB vs GraphDB vs Oxigraph. Use when investigating nightly benchmark numbers, a QPS regression or A/B gap, the harness.sqlite trend DB, or the spb-256 nightly job.
---

# Nightly benchmarks

The `nightly` workflow (`.github/workflows/nightly.yml`) runs LDBC SPB-256 at 03:00 UTC
on the self-hosted `hornbench` runner. It serves the flat materialized closure
`spb-256.nt` (no reasoning) over SPARQL/HTTP and drives it with the LDBC SPB driver,
once against **HornDB**, once against **GraphDB Free**, and twice against **Oxigraph**
— from the store as bulk-loaded and from an `oxigraph optimize`d copy (the A/B
reference legs). The four legs run sequentially — each engine is up only for its own
leg, so it never competes with the others for RAM / OS page cache. Trigger ad-hoc with
`gh workflow run nightly.yml`.

## Metrics recorded (suite `ldbc-spb-256`)

Each leg records the **full SPB driver report** into the `metrics` table, keyed by
engine label in the `dataset` column (`horndb`, `graphdb-free`, `oxigraph`,
`oxigraph-optimized`; `rdfox` if that leg is run). The parser is `crates/harness/src/ldbc_spb.rs` (it scrapes the
driver's final cumulative block + header).

**Headline (always present)** — the stable reporting contract; `harness report
--metric <name>` and the README query these by name:

| metric_name      | units | meaning                                  |
|------------------|-------|------------------------------------------|
| `editorial-qps`  | ops   | editorial (write-side) operations/sec    |
| `aggregation-qps`| qps   | aggregation **read** queries/sec         |
| `duration-s`     | s     | wall-clock of the driver run             |

**Full report (also recorded; per-section, all from the final cumulative block).**
The `q<N>-*`, per-op timing, and error series exist only when the driver ran
**verbose** (the nightly scenario sets `verbose=true`); a non-verbose run records
the count-only subset. `<N>` is the aggregation query-type id (`q1`, `q2`, …):

| metric_name | units | meaning |
|---|---|---|
| `q<N>-count` | ops | executions of query type `Q<N>` |
| `q<N>-avg-ms` / `-min-ms` / `-max-ms` | ms | `Q<N>` execution time (verbose) |
| `q<N>-errors` | count | `Q<N>` failed executions (verbose) |
| `aggregation-total-queries` | ops | total retrieval queries |
| `aggregation-errors` | count | total aggregation errors (verbose) |
| `editorial-total-ops` | ops | total CW insert+update+delete ops |
| `editorial-{insert,update,delete}-count` | ops | per-op counts |
| `editorial-{insert,update,delete}-{avg,min,max}-ms` | ms | per-op timing (verbose) |
| `editorial-{insert,update,delete}-errors` | count | per-op errors (verbose) |
| `cw-count` | count | Creative Works in the dataset (header) |
| `reference-entities` | count | reference entities (header) |
| `geo-locations` | count | geo locations (header) |
| `completed-query-mixes` **or** `completed-query-runs` | count | whichever the driver reported |

> With `editorialAgents=0` (the current nightly scenario) the editorial series are
> present but zero; they become meaningful once editorial agents are enabled
> ([#125](https://github.com/sunstoneinstitute/horndb/issues/125)).

## The trend DB keeps a 90-day rolling window

The cumulative sqlite lives **on hornbench, outside the ephemeral checkout**, at
`$HARNESS_DB = /home/bench/horndb-bench/harness.sqlite`. Every harness invocation
(the engine legs + the report) reads `$HARNESS_DB` by default, so each night appends
its rows; a `harness prune --keep-days 90` step then drops runs older than 90 days.
Path precedence in the binary: `--db` > `$HARNESS_DB` > `target/harness.sqlite`.
The per-run artifact `harness-<run_id>` is a snapshot of that retained window.

> If you ever see only the latest run's points in the trend, the append chain is broken
> — check that `$HARNESS_DB` is set in the job env and points at the persistent path.

## Grab the artifact

```bash
gh run list --workflow nightly.yml -L 10                # find a run id
gh run download <run-id> -D /tmp/nightly                # pulls harness-<run-id>/harness.sqlite
```
Or pull the live cumulative DB straight from the runner (freshest, full history):
```bash
scp hornbench:/home/bench/horndb-bench/harness.sqlite /tmp/harness.sqlite
```

## Query the trend

The report subcommand (oldest-first series, optionally a GitHub-summary table+chart):
```bash
./target/release/harness report --suite ldbc-spb-256 --metric aggregation-qps
./target/release/harness report --suite ldbc-spb-256 --metric aggregation-qps --format markdown
```
Or hit sqlite directly — schema in `crates/harness/src/db.rs` (`runs`, `outcomes`, `metrics`):
```bash
# latest A/B for a metric
sqlite3 /tmp/harness.sqlite "
  SELECT m.dataset, m.metric_value, r.commit_sha, m.timestamp
  FROM metrics m JOIN runs r ON r.run_id = m.run_id
  WHERE m.suite='ldbc-spb-256' AND m.metric_name='aggregation-qps'
  ORDER BY m.timestamp DESC LIMIT 6;"

# full HornDB aggregation-qps history, chronological
sqlite3 /tmp/harness.sqlite "
  SELECT m.timestamp, m.metric_value
  FROM metrics m
  WHERE m.suite='ldbc-spb-256' AND m.metric_name='aggregation-qps' AND m.dataset='horndb'
  ORDER BY m.timestamp ASC;"
```

## Notes

- Run benchmarks **only** on hornbench (stable env) — see the run-benchmarks memory.
- Scenario/driver assets are a prepared tree at `$SPB_ASSETS` on the runner; the dataset
  is `spb-256.nt`. Scripts: `crates/harness/scripts/run-spb-256.sh` (HornDB),
  `run-graphdb-free-spb-256.sh` (GraphDB), `run-oxigraph-spb-256.sh` (Oxigraph).
- GraphDB / Oxigraph versions are pinned via `GRAPHDB_VERSION` / `OXIGRAPH_VERSION` in
  the workflow, not the runner's install; the per-run `start-*.sh` downloads the pinned
  build if absent (cached across runs).
- **Oxigraph precondition:** the persisted RocksDB stores (`spb-store` and the
  `oxigraph optimize`d `spb-store-optimized`) must be built once on the runner before
  the first leg — `DATASET=$SPB_ASSETS/spb-256.nt
  crates/harness/scripts/bootstrap-oxigraph-spb.sh` builds both. Until then each
  Oxigraph leg self-skips (`start-oxigraph.sh` exits non-zero, `continue-on-error`
  swallows it) and no `oxigraph`/`oxigraph-optimized` rows appear in the trend. Same
  one-time-bootstrap model as GraphDB.
