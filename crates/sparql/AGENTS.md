# `horndb-sparql` (SPEC-07) — agent notes

Parser (spargebra), algebra, planner, runtime, axum HTTP server (`server` feature,
on by default).

- Tracks the unified workspace versions (`oxrdf 0.3.x`, `oxrdfio 0.2.x`,
  `sparesults 0.3.x`) with `rdf-12` (and `sparesults/sparql-12`) features on
  workspace-wide after PR2 of the RDF 1.2 migration.
- Additionally enables `spargebra/sep-0006` (for `GraphPattern::Lateral`) and
  `spargebra/sparql-12` (for `TermPattern::Triple`).
- Triple-term patterns are accepted only when callers pass `SparqlConfig::rdf12()`;
  the default config rejects them so SPARQL 1.1 callers keep their semantics. See
  `src/lib.rs::SparqlConfig` and `translate_query_with` / `execute_query_with`.
- Enabling `oxrdf/rdf-12` workspace-wide forces `oxigraph/rdf-12` too (sparopt /
  spareval need their `sparql-12` arms gated on, and Cargo only unifies features
  upward).
- `incremental` feature (default-on, SPEC-24 S4): `exec::circuit` wires a
  `horndb-incremental` `Circuit` behind `HornBackend` (`attach_circuit`). It
  links SuiteSparse:GraphBLAS through `horndb-closure`; `--no-default-features`
  builds without it. Tests: `tests/circuit_wiring.rs`, and the
  `circuit-delete-01` case in `tests/w3c_update_suite.rs`.
- HTTP server tests: `cargo test -p horndb-sparql --features server` — required for
  a full SPARQL pass.
- `serve --data` loads `.nt`/`.ttl`/`.nq`/`.trig` (HDB-112): `.nq`/`.trig` are dataset
  (quad) formats — each quad loads into the named graph it carries, not the default
  graph. `src/bin/serve.rs::load_file` routes them through `update::parse_rdf_bytes`,
  the same parser call site SPARQL `LOAD` uses (`update.rs::fetch_and_parse`), so the
  two never drift on format handling. `--materialize` does not yet support these two
  formats (it collapses everything into one `oxrdf::Dataset` default graph before
  closure) and refuses them at startup rather than silently dropping the graph split.
- `server::Limits` (HDB-118) is admission control for `/query` and GSP `GET
  /graphs`, plus the `/query`+`/update`+`/graphs` request-body cap. A permit is
  taken in `query.rs::run` before either execution path and **moved into the
  `spawn_blocking` closure**
  on the streaming path, so it is held until the client has drained the body —
  that task owns a blocking-pool thread, the store read guard and the operator
  tree for the whole stream. Releasing at first chunk would cap nothing. If you
  add another store-touching route, gate it the same way.

## Graph Store Protocol (`/graphs`, SPEC-28 S5)

`server/graph_store.rs` serves the four Graph Store Protocol verbs on one route,
`/graphs`, with the graph selected by `?graph=<iri>` or `?default`. Read that
module's header comment before changing it — three rules are load-bearing:

- **`PUT` diffs against base quads only.** The read set is
  `Store::scan_graph_quads`, which reads asserted quads straight out of storage
  with no reasoning seam, so a `PUT` never deletes a derived quad (SPEC-29 D5).
  An empty diff commits nothing.
- **Reserved graphs are read-only over GSP** — the write verbs call
  `update.rs::reserved_iri_write_check`, the same closed-namespace rule SPARQL
  Update uses. Do not re-implement it.
- **`?default` writes are refused on a `--materialize` store**, because
  `load_with_reasoning` puts asserted and inferred triples in the default graph
  indistinguishably. The flag is a one-way process-global (`flag_materialized`),
  like the inconsistency flag — so its test needs its own test binary
  (`tests/graph_store_materialized.rs`).

The `sparql11-gsp` conformance suite key and the live-server harness kind that
drives the W3C `http-rdf-update/` cases are **not** here; they are HDB-165.

## Operational endpoints (HDB-124)

Alongside `/query`, `/update`, `/graphs`, and `/metrics` (`server/mod.rs`), the
router serves the basics a Kubernetes deployment needs on day one:

- **`GET /healthz`** — always `200`. Proves the process is up and the axum event
  loop answers requests; it does NOT mean the data is loaded. Wire it to a
  liveness probe.
- **`GET /readyz`** — `200` once the `serve` binary's startup data load (and any
  `--materialize` pass) finishes, `503` before that. Backed by `AppState.ready`
  (an `Arc<AtomicBool>`), flipped once in `bin/serve.rs` at the end of
  `run_load`. Wire it to a readiness probe — a real corpus with no persistence
  yet takes minutes to load, and the pod must stay out of the load-balancer
  pool for that whole window, not just refuse connections.
- **Every response carries `x-request-id`** — passed through from the request
  header if the caller sent one, else generated as `<pid>-<seq>` from a
  process-wide monotonic counter (`server/request_id.rs`; no `uuid`/`rand` dep,
  a log-correlation id only needs to be unique for the life of the process).
  The same id appears in the `serve: <method> <path> <status> <ms>ms
  request_id=<id>` access-log line the `record_request` middleware
  (`server/mod.rs`) `eprintln!`s for every request, so a slow query in the log
  can be matched back to the client that reported it.
- **Graceful shutdown**: `bin/serve.rs::main` binds the listener and starts
  `axum::serve` BEFORE the data load, spawning the load onto a
  `spawn_blocking` thread — this is what lets `/healthz`/`/readyz` answer
  during a multi-minute cold load instead of refusing every connection until
  it finishes. On SIGTERM/SIGINT it stops accepting new connections
  (`with_graceful_shutdown`) and gives in-flight requests up to
  `[server].shutdown_drain` (default `30s`; override via `--shutdown-drain` or
  `HORNDB_SERVER__SHUTDOWN_DRAIN`, SPEC-26 layering) to finish, via
  `tokio::time::timeout` racing the server task — past the deadline the
  process force-exits (code 1) rather than hang. A load failure after the
  socket is already bound is fatal (`std::process::exit(1)`), matching the
  pre-HDB-124 behavior of failing the whole process on a bad load.

## Aggregation perf profiling

`examples/agg_profile.rs` is the diagnostic harness for the aggregation-qps
investigation (the "12-vs-150 agg-qps" work): it synthesises an SPB-ish graph and
times `COUNT` / `GROUP BY` / `DISTINCT` with ablations that isolate the per-row
`String`-materialization tax from the WCOJ join. It is **not** a recorded bench, so
it is fine to run on the laptop — recorded numbers come from the SPB-256 nightly.

```bash
cargo run -p horndb-sparql --release --example agg_profile -- [works]
HORNDB_EXEC_PHASES=1 cargo run -p horndb-sparql --release --example agg_profile
```

With `HORNDB_EXEC_PHASES=1` each timed query also prints its per-operator
exec-phase split (HDB-109), diffed across that query's own timed loop so it
never borrows a neighbour's work. Same phases as `docs/metrics.md`. Still a
laptop tool — recorded phase numbers come from
`scripts/bench/exec-phases.sh` on hornbench.

See `INTEGRATION-NOTES.md` for design decisions.
