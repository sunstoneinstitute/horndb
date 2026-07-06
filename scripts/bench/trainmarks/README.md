# HornDB × trainmarks

Run the [DataTreehouse **trainmarks**](https://github.com/DataTreehouse/trainmarks)
RDF benchmark against HornDB's storage/WCOJ SPARQL backend (`HornBackend`).

trainmarks is a synthetic **e-commerce** graph (customers / orders / products)
at three scales and six SPARQL queries, measuring I/O (Turtle + N-Triples
read/write) and query throughput. **No OWL reasoning.** Unlike the RDFox
comparison, trainmarks is a public, permissively-licensed benchmark with **no
DeWitt-style clause**, so these numbers may be committed and published.

## Quick start

```bash
scripts/bench/trainmarks.sh                        # all three scales
scripts/bench/trainmarks.sh --scales medium,large  # subset
scripts/bench/trainmarks.sh --timeout 300          # per-op timeout (seconds)
```

Run it on the `hornbench` server (see `BENCHMARKS.md`) for any number you intend
to record. Output: `target/trainmarks/results_horndb.json` (gitignored scratch),
in the upstream per-framework JSON schema — copy it into a checkout of the
upstream report tree (`results/`) to render the charts alongside other engines.

## What gets measured

Per scale (`medium` ~100K / `large` ~1M / `xlarge` ~10M triples), in order:

| operation | what HornDB does |
|---|---|
| `read_turtle` | parse `<scale>.ttl` → fresh `HornBackend` (this store feeds the queries) |
| `write_turtle` | serialize the store back to Turtle (`HornBackend::iter_oxrdf` → `oxttl`) |
| `write_ntriples` | serialize the store back to N-Triples |
| `read_ntriples` | parse `<scale>.nt` → fresh `HornBackend` (discarded) |
| `query_qN_cold` | first (cold) run of query N |
| `query_qN` | best of three warm runs of query N |

Each operation has a wall-clock timeout (default 600s, matching upstream). A
watchdog records `"TIMEOUT"` for an over-running operation and exits the
process, so a pathological query can't hang the suite. The driver runs **one
process per scale** (bounded peak memory; clean watchdog exit), driven by
`trainmarks.sh`.

## The six queries

Vendored verbatim from upstream under `queries/` (so the bytes match what other
engines run):

- **q1** `COUNT(*)` over all triples.
- **q2** customer spend: `GROUP BY` + `COUNT`/`SUM`, `ORDER BY DESC`, `LIMIT 20`.
- **q3** 3-entity join filtered to Norwegian customers, `ORDER BY DESC`, `LIMIT 50`.
- **q4** revenue by country/segment with `OPTIONAL` + `COUNT(DISTINCT …)` — the
  heaviest query (left-join over all orders); the most likely to time out at
  `xlarge`.
- **q5** `CONSTRUCT` of the Norwegian-customer subgraph.
- **q6** conditional `DELETE`/`INSERT … WHERE` with nested `IF` price arithmetic.

## Files

- `generate_data.py` — vendored upstream generator (fixed seed 42, reproducible).
  Writes `data/{medium,large,xlarge}.{ttl,nt}` and the query files.
- `queries/*.rq` — vendored upstream SPARQL queries.
- `../trainmarks.sh` — the runner (generate → build → run each scale).
- driver: `crates/bench-trainmarks` (`bench-trainmarks` binary).

## Caveats

- **Provenance.** `generate_data.py` and `queries/` are copied verbatim from
  the upstream repo; re-sync them if upstream changes.
- **`HornBackend`, not the Python bindings.** The `horndb_rdflib` PyO3 bindings
  wrap the in-memory `MemStore`, which does not scale past ~500K triples; this
  driver uses the storage/WCOJ `HornBackend` so it reaches `xlarge`.
- **SUM type promotion.** HornDB's `SUM` over `xsd:double` currently yields
  `xsd:decimal` (value correct, datatype differs) — noted as a follow-up.
- **Hardware fingerprint matters.** Compare only within one machine.
