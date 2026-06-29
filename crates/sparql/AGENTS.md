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
- HTTP server tests: `cargo test -p horndb-sparql --features server` — required for
  a full SPARQL pass.

## Aggregation perf profiling

`examples/agg_profile.rs` is the diagnostic harness for the aggregation-qps
investigation (the "12-vs-150 agg-qps" work): it synthesises an SPB-ish graph and
times `COUNT` / `GROUP BY` / `DISTINCT` with ablations that isolate the per-row
`String`-materialization tax from the WCOJ join. It is **not** a recorded bench, so
it is fine to run on the laptop — recorded numbers come from the SPB-256 nightly.

```bash
cargo run -p horndb-sparql --release --example agg_profile -- [works]
```

See `INTEGRATION-NOTES.md` for design decisions.
