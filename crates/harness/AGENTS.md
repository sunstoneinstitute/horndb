# `horndb-harness` (SPEC-01) — agent notes

Conformance + benchmark runner. Ships the `harness` binary and loads
`harness/selected.toml` from the workspace root. See also `README.md` in this crate.

The harness-first rule (SPEC-00): a SPEC is not satisfied until its referenced
subset in this harness is green. Implementation work may *grow* a subset but never
bypass it.

## Building the binary

`cargo build -p horndb-harness --bin harness [--release] [--features real-engine]`

Two engines:

- `--engine stub` — no real engine, for harness plumbing tests.
- `--engine owlrl` — the real engine. Requires `--features real-engine` at build time.

## Typical local runs

```bash
# Stage 0 / plumbing only
cargo run -p horndb-harness --bin harness -- --engine stub run --allow-failing

# Stage 1 real engine, full 50-case OWL 2 RL subset (fetches W3C suites first)
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness --features real-engine -- --engine owlrl run

# Trend report from prior runs (SQLite-backed)
cargo run -p horndb-harness --bin harness -- report --suite ldbc-spb-256 --metric editorial-qps
```

Harness state lives in `target/harness.sqlite`; CI publishes JUnit to
`target/junit.xml`. Fetched corpora go under `crates/harness/data/` (gitignored).

**`run` has no `--suite` filter** — it always executes the whole `selected.toml`
set. `--suite` is a `report`-only flag (the `report --suite ldbc-spb-256` example
above seeds the wrong guess). To narrow what `run` executes, edit
`harness/selected.toml`. `--engine` is a *global* flag and goes **before** the
`run`/`report` subcommand.

**GraphDB bench-runner scripts must use a `pkill` pattern that can't self-match**
(e.g. `graphdb-[0-9]`, matching the server JVM's `-Dgraphdb.dist=…/graphdb-<ver>`).
Linux `procps pkill -f` matches the start script's own argv and SIGTERMs it
(exit 143); macOS BSD `pkill` spares the caller, so a self-matching pattern is a
silent false-pass locally that only fails on the Linux bench host.

## Selection file

The canonical selection file is `harness/selected.toml` at the workspace root. It
carries both the manifest-driven `[suites.*]` entries the harness binary loads and
the path-based `[sparql_query]` section consumed by `crates/sparql/tests/w3c_suite.rs`.

## Suite keys (`src/runner.rs`)

`owl2`, `owl2-w3c-rl`, `sparql11`, `sparql11-syntax`, `rdf12-n-triples`.

`rdf12-n-triples` runs the W3C RDF 1.2 N-Triples *syntax* tests (4 positive
`<<( s p o )>>` cases + 6 bad-syntax negatives); it uses
`TestKind::SyntaxPositive` / `SyntaxNegative` and invokes `oxttl::NTriplesParser`
directly with no reasoner involvement. Fixtures live under
`crates/harness/tests/fixtures/rdf12-n-triples/`, re-fetchable via
`crates/harness/scripts/fetch-w3c-suites.sh`. Upstream URL:
`https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax/` — note the
`syntax/` segment; the top-level `rdf-n-triples/manifest.ttl` only `mf:include`s the
syntax sub-manifest alongside `c14n/` and the RDF 1.1 N-Triples suite.

`sparql11-syntax` runs the W3C SPARQL 1.1 *syntax* tests — query (`.rq`) and update
(`.ru`) forms, both positive and negative. The manifest uses the mf:* test types
`PositiveSyntaxTest11` / `NegativeSyntaxTest11` / `PositiveUpdateSyntaxTest11` /
`NegativeUpdateSyntaxTest11` (whose `mf:action` points directly at the query/update
file, with no `qt:QueryTest` blank node). Cases are graded by **`spargebra`** — the
same parser the SPEC-07 engine uses — via `TestKind::SparqlSyntaxPositive` /
`SparqlSyntaxNegative`: a positive case passes iff parsing succeeds, a negative case
passes iff parsing fails. No data, no result set, no reasoner. Fixtures are a curated,
checked-in subset under `crates/harness/tests/fixtures/sparql11-syntax/` (stable IDs,
no large corpus), so the suite runs in sub-milliseconds with no network at CI time —
it fits the SPEC-01 NF1 per-PR budget. Upstream source the subset is drawn from:
`https://www.w3.org/2009/sparql/docs/tests/` (`syntax-query/`,
`syntax-update-1/`, `syntax-update-2/`). To grow it, add cases to that fixture dir +
`harness/selected.toml`; the manifest reader and runner already understand the test
types (issue #110, part of the SPEC-01 harness epic #10).
