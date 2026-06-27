---
name: nightly-benchmarks
description: Work with HornDB's nightly LDBC SPB-256 benchmark — find runs, grab the cumulative sqlite artifact, and query the trend series (editorial-qps, aggregation-qps, duration-s) for HornDB vs GraphDB. Use when investigating nightly benchmark numbers, a QPS regression or A/B gap, the harness.sqlite trend DB, or the spb-256 nightly job.
---

# Nightly benchmarks

The `nightly` workflow (`.github/workflows/nightly.yml`) runs LDBC SPB-256 at 03:00 UTC
on the self-hosted `hornbench` runner. It serves the flat materialized closure
`spb-256.nt` (no reasoning) over SPARQL/HTTP and drives it with the LDBC SPB driver,
once against **HornDB** and once against **GraphDB Free** (A/B). Trigger ad-hoc with
`gh workflow run nightly.yml`.

## Metrics recorded (suite `ldbc-spb-256`)

Each leg records three metrics into the `metrics` table, keyed by engine label in the
`dataset` column (`horndb`, `graphdb-free`; `rdfox` if that leg is run):

| metric_name      | units | meaning                                  |
|------------------|-------|------------------------------------------|
| `editorial-qps`  | ops   | editorial (write-side) operations/sec    |
| `aggregation-qps`| qps   | aggregation **read** queries/sec         |
| `duration-s`     | s     | wall-clock of the driver run             |

## The trend DB is append-only

The cumulative sqlite lives **on hornbench, outside the ephemeral checkout**, at
`$HARNESS_DB = /home/bench/horndb-bench/harness.sqlite`. Every harness invocation
(both engine legs + the report) reads `$HARNESS_DB` by default, so each night appends
its rows. Path precedence in the binary: `--db` > `$HARNESS_DB` > `target/harness.sqlite`.
The per-run artifact `harness-<run_id>` is a snapshot of that full cumulative DB.

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
  `run-graphdb-free-spb-256.sh` (GraphDB).
- GraphDB version is pinned via `GRAPHDB_VERSION` in the workflow, not the runner's install.
