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

Two query-evaluation gates coexist. `[suites.sparql11-eval]` (below) is the
manifest-driven one and covers the full W3C suite. `[sparql_query]` is the older
path-based one, consumed by `crates/sparql/tests/w3c_suite.rs` against
hand-mirrored fixtures; it stays because it also exercises the
`default_graph`-mode dimension the upstream manifests do not express. Each
`[sparql_query]` entry names a fixture dir holding `query.rq`, `form`, `expected.srj`,
and its data as either `data.nt` (default graph) or `data.trig` (named graphs),
plus an optional `default-graph` file selecting the `default_graph` mode
(SPEC-28 D2). W3C cases that are mirrored but cannot pass are listed with their
reason in `harness/KNOWN-MANIFEST-BUGS.md`.

## Suite keys (`src/runner.rs`)

`owl2`, `owl2-w3c-rl`, `sparql11`, `sparql11-eval`, `sparql11-gsp`,
`sparql11-syntax`, `rdf12-n-triples`.

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

`sparql11-eval` runs the W3C SPARQL 1.1 **evaluation** suite —
`mf:QueryEvaluationTest` + `mf:UpdateEvaluationTest`, graded by executing the
real SPEC-07 engine (`horndb-sparql`, `default-features = false`) and comparing
against the case's expected result. Unlike every other suite it is **not**
mirrored into fixtures: `harness/selected.toml` points at
`crates/harness/data/w3c-sparql11-tests/sparql11-test-suite/manifest-all.ttl`,
and the manifest reader follows its `mf:include` list depth-first. So
`crates/harness/scripts/fetch-w3c-suites.sh` must have run first. CI's
conformance job runs the script with `HARNESS_BIN=./target/conformance/harness`
so it reuses the already-built binary instead of a second debug `cargo run`.

`sparql11-gsp` runs the W3C SPARQL 1.1 **Graph Store Protocol** suite (SPEC-28
S5). It is the only suite that does not grade files. Each
`mf:GraphStoreProtocolTest` is an ordered sequence of HTTP requests, and what
one request leaves in the store is what the next asserts on, so
`TestKind::GraphStoreProtocol` runs the whole sequence against one live server:
`src/gsp.rs` calls `horndb_sparql::server::build_router` over a `HornBackend`
(the storage path `serve` uses; `MemStore` keeps only a lexical form per term,
so blank nodes would come back out of it as IRIs), binds `127.0.0.1:0` so the
kernel picks a free port, and speaks HTTP/1.1 over a plain `TcpStream` —
`Connection: close` on every request makes the response "read to EOF", so there
is no chunked or keep-alive framing to implement. One server per case, so state
carries within a case and never between cases.

Two mappings the upstream manifest asks a runner to make, both in `gsp.rs`:
every `ht:absolutePath` starts with `/gsp`, rewritten to HornDB's `/graphs`;
and a response body is compared **graph-isomorphically** (parse both sides,
`Graph::canonicalize`), not byte-wise, since the payloads are full of blank
nodes.

Corpus: `graph-store-protocol/{manifest,manifest-direct,manifest-indirect}.ttl`
from the `rdf-tests` mirror. Note this is **not** the `http-rdf-update/`
directory SPEC-28 names — that one holds no machine-readable tests (a prose
`tests.txt` in the tarball; every case in the mirror's manifest is
`dawg:Deprecated` with its request/response inside a Markdown `rdfs:comment`,
pointing at `graph-store-protocol/`). The replacement keeps the same
`http-rdf-update/manifest#` case IRIs, which is why the ids still read that way.
The manifest reader understands the W3C HTTP-in-RDF vocabulary for these:
`ht:Connection`/`ht:requests`/`ht:Request`/`ht:resp`, `ht:headers` with
`ht:fieldName`/`ht:fieldValue`, `ht:body [ cnt:chars … ]`, and
`mf:expectedStatus` naming `hts:` status IRIs (an unrecognised one is a hard
error, never a silently-dropped assertion). Like `sparql11-eval` the corpus is
fetched, not mirrored, so the entry carries `fetched = true`.

### Fetched corpora: `fetched = true` and `--require-corpus`

A suite whose manifest comes from a fetched corpus rather than a checked-in
fixture sets `fetched = true` in its `selected.toml` entry (`sparql11-eval`
and `sparql11-gsp`). It changes what happens when that manifest is **absent**:

- default (`harness run`) — the suite reports **Skipped** with a reason naming
  the missing path and `fetch-w3c-suites.sh`, and every other selected suite
  still grades. Jobs that never fetch (CI's `tests` job, a fresh clone) are
  therefore unaffected; without this the whole run aborted with exit 2 before
  grading a single case.
- `harness run --require-corpus` — the same missing manifest is a **hard
  error**. CI's conformance job and the nightly W3C step pass it, because they
  *do* fetch: a suite that silently stopped being graded there would be worse
  than a red one (SPEC-00 harness-first rule).

Suites without `fetched = true` are unchanged: their manifest is checked in, so
a missing one is still an immediate error.

Grading details (`src/sparql_eval.rs`): every file IRI is a local `file://` IRI
and each query/update gets one `BASE <file://…>` line prepended, so a relative
IRI in the query (`GRAPH <exists02.ttl>`) resolves to the same IRI the
`qt:graphData` file was loaded under. `qt:data` becomes the default graph and
`qt:graphData` the named graphs, so queries run in `DefaultGraphMode::Strict`.
Expected results are read from `.srx`/`.srj` via `sparesults`; other extensions
report `result format not graded yet: …` (a visible red, not a silent pass).
Solutions compare as a set of variables plus a sorted multiset of rows, with an
explicit `xsd:string` datatype normalised away on both sides, and **numeric
literals compared by value within their datatype** — `"3"` and `"3.0"` are the
same `xsd:decimal`, `"2E-1"` and `"2.0E-1"` the same `xsd:double`, while
`xsd:integer` 3 and `xsd:decimal` 3 still differ. The upstream `.srx` files
were written by several engines over a decade and spell the same value more
than one way; two cases in the same suite even disagree (`functions/ceil01`
wants `"3"` where `aggregates/agg-avg-02` wants `"2.0"`), so no single output
spelling can satisfy both. An engine panic on an upstream query is caught and
graded as an ordinary failure.

### `expected_failures` — the known-failure allowlist

`include = ["*"]` selects every case in the tree; SPEC-00's harness-first rule
forbids narrowing a suite to make a run look better. A case that cannot pass
today is instead listed in the suite's `expected_failures` array. The runner
rewrites its outcome:

- listed **and failing** → **Skipped**, reason prefixed `known failure: `;
- listed **and passing** → **Failed**, "listed in expected_failures but passed —
  drop it from harness/selected.toml";
- not listed → unchanged.

That is strictly stronger than a pass-count floor: it catches drift in *both*
directions and names the exact case. The per-PR conformance job therefore stays
green on documented gaps but goes red on any real regression, and a fix cannot
land without its allowlist line being removed in the same change. Patterns match
a case IRI by exact match, suffix, or `prefix*` substring (`selected::pattern_matches`).

The grouped root-cause triage behind those entries lives in
`harness/KNOWN-MANIFEST-BUGS.md`.

### Pass-count trend

Every `harness run` records `passed` and `selected` per suite into the SQLite
trend DB (`target/harness.sqlite`, or `$HARNESS_DB`). Because the allowlist keeps
the suite green, the pass count is the number to watch over time; the nightly
workflow runs the suite and publishes
`harness report --suite sparql11-eval --metric passed` into its step summary.
These are trend-DB rows, not Prometheus series — they do not belong in
`docs/metrics.md`.
