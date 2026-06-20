# Design spec — pyoxigraph-shaped native Python `Store` (SPEC-10 extension)

> Status: **implemented** (first increment). Extends
> [`SPEC-10`](SPEC-10-rdflib-compatible-python-api.md). Plan:
> [`docs/plans/2026-06-18-...`] → [`docs/plans/2026-06-20-python-native-store.md`].

## Why

SPEC-10's first increment shipped an **rdflib-shaped** `Graph` facade over a
single default graph of triples. The motivating downstream consumer —
`sunstoneinstitute/rdf-registry`'s build pipeline — does **not** drive rdflib;
it drives the **`pyoxigraph.Store` API**: a quad store with named graphs,
`store.quads_for_pattern(s, p, o, g)`, `store.load(..., to_graph=)`, and
`store.query(..., use_default_graph_as_union=True)`. The discovery query loads
one named graph per source file and queries their union. None of that fits the
default-graph-only rdflib facade.

The chosen design (confirmed with the maintainer) blends the three reference
APIs:

- **pyoxigraph** → the *spine*: a native quad `Store` with named graphs, value
  objects, and pyoxigraph-shaped `QuerySolutions`/`QuerySolution`.
- **rdflib** → a *compat skin*: the existing `URIRef`/`BNode`/`Graph` facade,
  relocated under a `horndb.rdflib` submodule.
- **maplib** → *deferred*: DataFrame-shaped results (`.to_polars()`) are out of
  scope for this increment but the result object keeps a clean `variables` +
  row-iteration shape so a future **sunstone-py** integration can consume it
  without re-plumbing.
- **HornDB's own differentiator** → an explicit `Store.materialize()` that runs
  OWL 2 RL forward chaining and makes entailed triples queryable — something
  neither pyoxigraph nor rdflib offers natively.

## Packaging

The extension module is renamed `horndb_rdflib` → **`horndb`**. The native
pyoxigraph-shaped classes are the **top-level** `horndb.*` names; the rdflib
facade moves to a **`horndb.rdflib`** submodule. This resolves the `Literal` /
`Variable` name clash (both libraries use those names with different
semantics) and makes the ergonomic import `from horndb import Store, NamedNode`.

## Scope (this increment)

In scope — native `horndb` surface, modelled on pyoxigraph:

- Term value objects: `NamedNode`, `BlankNode`, `Literal`, `DefaultGraph`,
  `Triple`, `Quad`, `Variable`, each with `.value` and pyoxigraph-style
  `str()`/equality/hashing.
- `Store`: `add`/`remove`/`__contains__`/`__len__`/`__iter__`,
  `quads_for_pattern(s, p, o, graph_name)`, `named_graphs()`,
  `load(data, format, to_graph=)`, `serialize(format, from_graph=)`,
  `query(q, use_default_graph_as_union=)`, `update(q)`, `clear()`.
- Named graphs: default graph + IRI/blank-named graphs, faithfully modelled on
  the non-SPARQL surface.
- `RdfFormat`: Turtle, N-Triples, **N-Quads, TriG**, RDF/XML — the quad formats
  let named graphs survive a `load`/`serialize` round-trip.
- `QuerySolutions` / `QuerySolution`: iterate rows; index a solution by
  variable name (`sol["x"]`), position (`sol[0]`), or `Variable`; `.get()`.
- `Store.materialize()`: OWL 2 RL forward chaining (SPEC-04, pure-Rust
  RuleFiring backend — **no GraphBLAS dependency in the wheel**), inserting
  entailed triples into the default graph; `clear_inferred()` drops them.
- The rdflib facade unchanged in behaviour, served from `horndb.rdflib`.

Out of scope (follow-ups, tracked in `TASKS.md`):

- **Graph-scoped SPARQL** (`GRAPH ?g { … }`, `FROM` / `FROM NAMED`): the
  Stage-1 SPARQL executor is triple-only and flattens graph scope. `query`
  therefore exposes only the binary `use_default_graph_as_union` knob (default
  graph, or union of all graphs). Named-graph SPARQL scoping is gated on the
  SPARQL frontend's named-graph work.
- DataFrame results (`.to_polars()`/`.to_pandas()`) and DataFrame→RDF mapping
  (the maplib ideas) — deferred pending the sunstone-py integration.
- Streaming/lazy results (`materialize`-everything today), GIL release on hot
  scans, the multi-version wheel matrix, a `pyoxigraph`-drop-in import alias.

## Interface

```python
from horndb import Store, NamedNode, Literal, Quad, DefaultGraph, RdfFormat

store = Store()
store.load(ttl_bytes, RdfFormat.TURTLE, to_graph=NamedNode("file:core.ttl"))
store.add(Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), Literal("v")))

for q in store.quads_for_pattern(None, None, None, DefaultGraph()):
    ...

for sol in store.query(DISCOVER_RQ, use_default_graph_as_union=True):
    uri, kind = sol["uri"].value, sol["type"].value

asserted, inferred = store.materialize()    # OWL 2 RL closure

from horndb.rdflib import Graph, URIRef     # rdflib facade still available
```

## Acceptance criteria

1. A `Store` models the default graph plus IRI/blank named graphs;
   `quads_for_pattern` filters by subject/predicate/object **and** graph
   (`None` = wildcard; `DefaultGraph()` = default only), matching pyoxigraph.
2. `load`/`serialize` round-trip named graphs through N-Quads / TriG; triple
   formats flatten to the default graph; `to_graph=` overrides the target.
3. `query(..., use_default_graph_as_union=True)` evaluates over the union of
   all graphs; the default (`False`) evaluates over the default graph only.
   The exact `rdf-registry` discovery query (UNION + `BIND`) returns the
   expected rows.
4. `QuerySolution` is indexable by variable name, position, and `Variable`;
   `.value` exposes lexical content on terms; ASK → `bool`, CONSTRUCT → list
   of `Triple`.
5. `Store.materialize()` makes OWL 2 RL entailments (`rdfs:subClassOf` +
   `rdf:type` ⇒ inferred type) queryable; it is idempotent and
   `clear_inferred()` restores the asserted base. The wheel builds **without**
   SuiteSparse:GraphBLAS.
6. The rdflib facade continues to pass its differential suite from
   `horndb.rdflib`; the pure-Rust core (`quadstore`, `term`, `graph`) is
   unit-tested by `cargo test` with no Python interpreter.
7. The native surface is covered by `crates/python/tests/test_native_store.py`,
   including an optional differential check against `pyoxigraph`.

## Divergences (documented, not silently approximated — NF4)

- `Literal.datatype` always returns a `NamedNode` (effective datatype:
  `xsd:string` / `rdf:langString` for the implicit cases), matching pyoxigraph
  — **not** rdflib's `None` for plain literals. (The `horndb.rdflib.Literal`
  facade keeps rdflib's `None` behaviour.)
- `GRAPH`-scoped query patterns are not graph-isolated (engine limitation,
  above). `quads_for_pattern` *is* fully graph-aware.
- `Store.update(...)` operates on the **default graph**; named-graph update
  targets are a follow-up. Inferred triples in the default graph are dropped by
  an update (re-`materialize()` afterwards).
