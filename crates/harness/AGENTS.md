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

## Selection file

The canonical selection file is `harness/selected.toml` at the workspace root. It
carries both the manifest-driven `[suites.*]` entries the harness binary loads and
the path-based `[sparql_query]` section consumed by `crates/sparql/tests/w3c_suite.rs`.

## Suite keys (`src/runner.rs`)

`owl2`, `owl2-w3c-rl`, `sparql11`, `rdf12-n-triples`. The last runs the W3C RDF 1.2
N-Triples *syntax* tests (4 positive `<<( s p o )>>` cases + 6 bad-syntax negatives);
it uses `TestKind::SyntaxPositive` / `SyntaxNegative` and invokes
`oxttl::NTriplesParser` directly with no reasoner involvement. Fixtures live under
`crates/harness/tests/fixtures/rdf12-n-triples/`, re-fetchable via
`crates/harness/scripts/fetch-w3c-suites.sh`. Upstream URL:
`https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax/` — note the
`syntax/` segment; the top-level `rdf-n-triples/manifest.ttl` only `mf:include`s the
syntax sub-manifest alongside `c14n/` and the RDF 1.1 N-Triples suite.
