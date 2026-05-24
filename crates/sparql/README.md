# horndb-sparql

SPARQL 1.1 frontend for the HornDB project. See
`specs/SPEC-07-sparql-frontend.md` for the full contract.

## Stage 1 status

Implemented:
- Parser via `spargebra`.
- Algebra translation: BGP, Join, LeftJoin, Filter, Union, Project,
  Distinct, Slice, OrderBy, Extend, Values.
- Planner: 1:1 lowering to `PhysicalPlan` (no cost model).
- Runtime: walks the plan against an `Executor` impl.
- Built-in `MemStore` executor (HashSet) for tests and local use.
- Query forms: SELECT, ASK, CONSTRUCT.
- Update: `INSERT DATA`, `DELETE DATA`.
- Property paths: `/` (sequence), `^` (inverse) only.
- Entailment regimes: simple, materialized OWL 2 RL (marker).
- Result formats: SPARQL JSON, CSV, TSV (XML deferred).
- HTTP `/query` and `/update` (axum, feature-gated `server`,
  default on).

Deferred (Future Work):
- DESCRIBE.
- Full update vocabulary (LOAD/CLEAR/DROP/INSERT WHERE/DELETE WHERE).
- Property paths `*`, `+`, `?`, `|`, `!`.
- GROUP BY / aggregates / HAVING / MINUS / SERVICE.
- Graph Store Protocol.
- EXPLAIN pragma.
- Backward-chained entailment mode.
- True streaming results.
- DBSP-routed updates via SPEC-06.

The full SPARQL 1.1 conformance gate is SPEC-01's responsibility;
this crate vendors a 5-test sanity subset in
`crates/harness/tests/fixtures/sparql11/` and runs it in
`tests/w3c_suite.rs`.

## Running

```bash
cargo test -p horndb-sparql --features server
```

To start the HTTP server in your own binary:

```rust
use horndb_sparql::server::{build_router, AppState};
use horndb_sparql::exec::mem::MemStore;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    let state = AppState { store: Arc::new(Mutex::new(MemStore::default())) };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```
