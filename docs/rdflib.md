# rdflib vs HornDB

This page is a translation guide for people who know `rdflib` and want to understand the current HornDB surface area. If you are implementing the Python compatibility layer, read [`SPEC-10`](specs/SPEC-10-rdflib-compatible-python-api.md) next.

HornDB is **not** a drop-in `rdflib` replacement. `rdflib` is a Python RDF toolkit with graph manipulation primitives, parsers, serializers, and a broad plugin ecosystem. HornDB is a Rust RDF reasoner and SPARQL engine with a narrower Stage 1 API surface and a stronger emphasis on reasoning, query planning, and provenance.

## Quick mapping

| Typical rdflib operation | rdflib idiom | HornDB equivalent today | Notes |
|---|---|---|---|
| Create a graph | `g = Graph()` | `MemStore::default()` or a backend implementing `Store` / `Executor` | Stage 1 ships the in-crate `MemStore`.
| Add a triple | `g.add((s, p, o))` | `Store::insert_triple(...)`, `MemStore::insert(...)`, or `INSERT DATA` via SPARQL | `MemStore` stores lexical-form strings.
| Remove a triple | `g.remove((s, p, o))` | `Store::delete_triple(...)` or `DELETE DATA` via SPARQL | Deletion is exact-match on the lexical form.
| Query triples | `g.query("SELECT ...")` | `horndb_sparql::api::execute_query(...)` or HTTP `/query` | SELECT, ASK, and CONSTRUCT are supported.
| Update triples | `g.update("INSERT DATA ...")` | `horndb_sparql::api::execute_update(...)` or HTTP `/update` | Stage 1 supports `INSERT DATA` and `DELETE DATA`.
| Iterate a pattern | `g.triples((s, p, o))` | `Executor::scan_bgp(...)` underneath the SPARQL planner | This is a backend seam, not the primary public API.
| Serialize query results | `g.query(...).serialize(...)` | SPARQL JSON / CSV / TSV response formats | Graph serialization of arbitrary stores is not a Stage 1 public feature.
| Serialize a graph | `g.serialize(format="turtle")` | No direct graph serializer in the current Stage 1 docs | Use SPARQL CONSTRUCT output or external tooling.
| Namespace helpers | `Namespace(...)`, `bind(...)` | SPARQL prefix declarations in query strings | There is no dedicated namespace manager exposed here.
| Reason over data | plugin / external reasoner | HornDB’s core purpose | Full OWL 2 RL reasoning is a project goal; Stage 1 is still the feasibility prototype.
| Open a remote endpoint | custom store / HTTP wrapper | `/query` and `/update` on the SPARQL server | The server is in `crates/sparql` and is feature-gated by `server`.

## Common workflows

### 1. Build a small in-memory dataset

`rdflib` style:

```python
from rdflib import Graph, URIRef

g = Graph()
g.add((URIRef("http://ex/s"), URIRef("http://ex/p"), URIRef("http://ex/o")))
```

HornDB style today:

```rust
use horndb_sparql::exec::mem::MemStore;

let mut store = MemStore::default();
store.insert((
    "http://ex/s".to_string(),
    "http://ex/p".to_string(),
    "http://ex/o".to_string(),
));
```

If you are already inside the SPARQL layer, the more idiomatic path is:

```rust
use horndb_sparql::api::execute_update;
use horndb_sparql::exec::mem::MemStore;

let mut store = MemStore::default();
execute_update(
    "INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> . }",
    &mut store,
)?;
```

### 2. Run a query

`rdflib` style:

```python
g.query("SELECT ?s WHERE { ?s <http://ex/p> <http://ex/o> }")
```

HornDB style:

```rust
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;

let mut store = MemStore::default();
store.insert((
    "http://ex/s".into(),
    "http://ex/p".into(),
    "http://ex/o".into(),
));

match execute_query("SELECT ?s WHERE { ?s <http://ex/p> <http://ex/o> }", &store)? {
    QueryAnswer::Solutions { vars, rows } => {
        assert_eq!(vars, vec!["s"]);
        assert!(!rows.is_empty());
    }
    _ => unreachable!(),
}
```

Supported query forms today:

- `SELECT`
- `ASK`
- `CONSTRUCT`

`DESCRIBE` is explicitly unsupported in the Stage 1 API.

### 3. Apply updates

`rdflib` style:

```python
g.update("INSERT DATA { <a> <b> <c> }")
```

HornDB style:

```rust
use horndb_sparql::api::execute_update;
use horndb_sparql::exec::mem::MemStore;

let mut store = MemStore::default();
execute_update("INSERT DATA { <a> <b> <c> . }", &mut store)?;
execute_update("DELETE DATA { <a> <b> <c> . }", &mut store)?;
```

Current Stage 1 caveat: the supported update vocabulary is intentionally small. Full SPARQL Update is a later step in the SPARQL spec.

### 4. Use a server endpoint

`rdflib` often talks to a local graph object; HornDB can also expose a server.

HornDB HTTP surface today:

- `/query`
- `/update`

The server lives in `horndb_sparql::server` and uses the in-crate `MemStore` for Stage 1 examples and tests.

### 5. Reasoning expectations

If your `rdflib` mental model is "RDF graph as a bag of triples," adjust it here:

- HornDB’s primary job is reasoning, not just storage.
- The project contract is OWL 2 RL / SPARQL 1.1 oriented.
- The current codebase is still in Stage 1, so the public API is narrower than the long-term design.
- Query and update semantics are built around the Rust SPARQL frontend, not around Python graph objects.

## Practical translation table

| If you usually reach for `rdflib` because… | In HornDB you usually use… |
|---|---|
| You want a tiny throwaway graph in memory | `MemStore` |
| You want to issue SPARQL SELECT/ASK/CONSTRUCT | `execute_query(...)` or `/query` |
| You want to mutate triples with SPARQL Update | `execute_update(...)` or `/update` |
| You want to plug in a custom backend | `Executor` and `Store` traits |
| You want query planning / join execution details | SPARQL algebra + planner + executor internals |
| You want OWL 2 RL reasoning | HornDB’s core engine and specs, not `rdflib` |

## What is missing compared with rdflib

This is the short list of things you should not assume exist yet:

- A Python API.
- A general-purpose graph serializer surface.
- A wide plugin ecosystem for parsers, serializers, and stores.
- A full `Graph.parse()` equivalent in the current Stage 1 public docs.
- A complete SPARQL Update vocabulary.
- `DESCRIBE` query support.

## What to read next

- [`../crates/sparql/README.md`](../crates/sparql/README.md) for the current SPARQL feature list.
- [`../specs/SPEC-07-sparql-frontend.md`](../specs/SPEC-07-sparql-frontend.md) for the full frontend contract.
- [`../specs/SPEC-02-storage.md`](../specs/SPEC-02-storage.md) if you are working on persistence or triple representation.
- [`../CLAUDE.md`](../CLAUDE.md) for repo-wide working rules.

## Bottom line

Use `rdflib` as the conceptual reference for "RDF graphs and SPARQL," but use HornDB’s own APIs when you care about reasoning, query planning, or the Rust execution model.
