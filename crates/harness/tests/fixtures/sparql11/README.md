# SPARQL 1.1 Query Test Suite — selected Stage-1 subset

Five hand-picked tests from the W3C SPARQL 1.1 Query Test Suite
(https://w3c.github.io/rdf-tests/sparql/sparql11/), one per supported
algebra construct. The full suite belongs to SPEC-01 (the harness);
this directory is intentionally small.

| Test ID                         | Construct exercised        |
|---------------------------------|----------------------------|
| basic-001                       | single-pattern SELECT      |
| basic-002                       | DISTINCT                   |
| basic-003                       | FILTER (?x = <iri>)        |
| basic-004                       | OPTIONAL / LeftJoin        |
| basic-005                       | ASK true                   |

Each test directory contains:
* `query.rq`        — the SPARQL query
* `data.nt`         — the input dataset (N-Triples)
* `expected.srj`    — the expected SPARQL JSON Results
* `form`            — single line: `select` or `ask`

The harness (Task 17) iterates this directory, runs each query, and
diffs the JSON answer against `expected.srj` (parsed, set-compared —
binding order is not significant).
