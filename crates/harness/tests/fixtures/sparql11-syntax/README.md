# W3C SPARQL 1.1 syntax suite — curated subset (suite key `sparql11-syntax`)

A small, hand-curated subset of the W3C SPARQL 1.1 *syntax* test suites
(<https://www.w3.org/2009/sparql/docs/tests/> — `syntax-query/`,
`syntax-update-1/`, `syntax-update-2/`). These tests assert only that the
SPARQL 1.1 grammar **accepts** (positive) or **rejects** (negative) a query
(`.rq`) or update (`.ru`) — there is no data, no result set, and no reasoner
involvement.

The harness grades each case with `spargebra` (the same parser the SPEC-07
engine uses) via `TestKind::SparqlSyntaxPositive` / `SparqlSyntaxNegative`.
Because parsing is sub-millisecond and the fixtures are checked in, the suite
runs on every PR with no network fetch, inside the SPEC-01 NF1 budget.

## Cases

Positive (grammar must accept):

| ID                            | Form   | Exercises                       |
|-------------------------------|--------|---------------------------------|
| syntax-select-01              | query  | `SELECT *`                      |
| syntax-bind-01                | query  | `BIND(expr AS ?v)`              |
| syntax-aggregate-01           | query  | `GROUP BY` / `HAVING` / `COUNT` |
| syntax-subquery-01            | query  | sub-`SELECT`                    |
| syntax-propertypath-01        | query  | property path `+` / `*`         |
| syntax-values-01              | query  | inline `VALUES`                 |
| syntax-update-insertdata-01   | update | `INSERT DATA`                   |
| syntax-update-deletewhere-01  | update | `DELETE WHERE`                  |
| syntax-update-modify-01       | update | `DELETE/INSERT … WHERE`         |

Negative (grammar must reject):

| ID                       | Form   | Why it is invalid                         |
|--------------------------|--------|-------------------------------------------|
| syntax-bad-01            | query  | unclosed group graph pattern              |
| syntax-bad-02            | query  | `BIND(expr)` with no `AS ?var`            |
| syntax-bad-03            | query  | dangling `GROUP`                          |
| syntax-update-bad-01     | update | `INSERT DATA` containing a variable       |
| syntax-update-bad-02     | update | `LOAD` with no source IRI                 |

## Growing the subset

Add `.rq` / `.ru` files here, list them in `manifest.ttl` with the appropriate
`mf:*SyntaxTest11` type, and add their IDs to `[suites.sparql11-syntax]` in
`harness/selected.toml`. The manifest reader (`crates/harness/src/manifest.rs`)
and runner (`crates/harness/src/runner.rs`) already understand the four test
types. Part of the SPEC-01 harness epic (#10), increment #110.
