# rdflib vs HornDB

HornDB is **not** a drop-in `rdflib` replacement. `rdflib` is a Python RDF toolkit with graph manipulation primitives, parsers, serializers, and a broad plugin ecosystem. HornDB is a Rust RDF reasoner and SPARQL engine: a narrower API surface, with the weight on OWL 2 RL reasoning, query planning, and named-graph datasets. It speaks SPARQL 1.1 query and update, serves the Graph Store Protocol, and stores quads, not just triples.

If you know `rdflib` and want the closest thing to it in HornDB, use the Python binding — not the Rust API directly.

## The rdflib-compatible surface

The `horndb-rdflib` binding (`crates/python/`, PyO3/maturin, importable as `horndb_rdflib`) is HornDB's actual compatibility layer: rdflib-shaped terms (`URIRef`/`BNode`/`Literal`/`Variable`/`Namespace`), a `Graph` facade (`add`/`remove`/`set`/`triples`/`subjects`/`objects`/`value`/`__len__`/`__contains__`/iteration), `parse`/`serialize` for Turtle and N-Triples, SPARQL `query`/`update` passthrough, and `bind`/`namespaces`. It's differential-tested against upstream rdflib.

- What works today, and what's still missing: [`crates/python/README.md`](../crates/python/README.md).
- The contract it implements against: [`specs/SPEC-10-rdflib-compatible-python-api.md`](specs/SPEC-10-rdflib-compatible-python-api.md).
- Agent notes (build/test, layout, gotchas): [`crates/python/CLAUDE.md`](../crates/python/CLAUDE.md).

## Writing Rust against HornDB directly

There is no rdflib-shaped Rust API, and none is planned — the compatibility promise is scoped to the Python binding above. From Rust, call `horndb_sparql::api::execute_query`/`execute_update` and the `Executor`/`Store` traits directly. See [`specs/SPEC-07-sparql-frontend.md`](specs/SPEC-07-sparql-frontend.md) and [`../crates/sparql/README.md`](../crates/sparql/README.md).
