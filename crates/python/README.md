# horndb (Python API)

An ergonomic Python API for
[HornDB](https://github.com/sunstoneinstitute/horndb), implemented as a
PyO3/maturin binding over the Rust engine. Two surfaces:

- **`horndb`** — the native, [`pyoxigraph`](https://pyoxigraph.readthedocs.io/)-shaped
  spine: a quad `Store` with named graphs, `quads_for_pattern`, multi-format
  `load`/`serialize`, SPARQL passthrough, and an explicit OWL 2 RL
  `materialize()` step.
- **`horndb.rdflib`** — an [`rdflib`](https://rdflib.readthedocs.io/)-compatible
  facade (`URIRef`/`BNode`/`Graph`/…).

See [`SPEC-10`](../../docs/specs/SPEC-10-rdflib-compatible-python-api.md) and the
design spec
[`2026-06-20-pyoxigraph-style-python-store.md`](../../docs/specs/2026-06-20-pyoxigraph-style-python-store.md).

## Native `Store` (the pyoxigraph-shaped spine)

```python
from horndb import Store, NamedNode, Literal, Quad, DefaultGraph, RdfFormat

store = Store()
# One named graph per source file, then query their union — the rdf-registry idiom.
store.load(turtle_bytes, RdfFormat.TURTLE, to_graph=NamedNode("file:core.ttl"))
store.add(Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), Literal("v")))

len(store)                                              # quad count, all graphs
store.named_graphs()                                    # [NamedNode('file:core.ttl')]
list(store.quads_for_pattern(None, None, None, DefaultGraph()))

for sol in store.query(DISCOVER_RQ, use_default_graph_as_union=True):
    uri, kind = sol["uri"].value, sol["type"].value     # index by name or position

# OWL 2 RL reasoning — HornDB's differentiator. Entailed triples become queryable.
asserted, inferred = store.materialize()
store.query("ASK { <http://ex/pingu> a <http://ex/Bird> }")   # True after closure
```

Supported `RdfFormat`s: `TURTLE`, `N_TRIPLES`, `N_QUADS`, `TRIG`, `RDF_XML`
(the quad formats round-trip named graphs). `query` returns `QuerySolutions`
(SELECT), `bool` (ASK), or a list of `Triple` (CONSTRUCT).

## rdflib-compatible facade (`horndb.rdflib`)

```python
from horndb.rdflib import Graph, URIRef, Literal, Namespace

EX = Namespace("http://ex/")
g = Graph()
g.add((EX.alice, EX.knows, EX.bob))
g.add((EX.alice, EX.name, Literal("Alice")))

len(g)                                  # 2
(EX.alice, EX.knows, EX.bob) in g       # True
list(g.objects(EX.alice, EX.name))      # [Literal('Alice')]

g.parse(data="<http://ex/x> <http://ex/p> <http://ex/y> .", format="nt")
print(g.serialize(format="turtle"))

res = g.query("SELECT ?o WHERE { <http://ex/alice> <http://ex/knows> ?o }")
for (o,) in res:
    print(o)                            # URIRef('http://ex/bob')

bool(g.query("ASK { <http://ex/alice> <http://ex/knows> <http://ex/bob> }"))  # True
g.update("INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> }")
```

Implemented (SPEC-10 functional requirements):

- **F1** term classes: `URIRef`, `BNode`, `Literal`, `Variable`, `Namespace`
  with rdflib-compatible equality, hashing, `str()`, and `n3()`.
- **F2** `Graph` lifecycle: `add`/`remove`/`set`, `triples`, `subjects`/
  `predicates`/`objects`, `value`, `__len__`, `__contains__`, iteration.
- **F4** `parse()` / `serialize()` for Turtle and N-Triples.
- **F5** `query()` / `update()` passthrough to the SPEC-07 SPARQL frontend; the
  `Result` object iterates SELECT rows, yields the ASK boolean via `bool()`,
  and iterates CONSTRUCT triples.
- **F6** namespace binding: `bind()`, `namespaces()`, `Namespace` term access.

## Not yet (later increments / Stage-2)

Graph-scoped SPARQL (`GRAPH` / `FROM` / `FROM NAMED` — the engine is triple-only
today, so `query` exposes only the `use_default_graph_as_union` knob), DataFrame
results (`.to_polars()`) + maplib-style DataFrame→RDF mapping (deferred for the
sunstone-py integration), rdflib `Dataset` / `ConjunctiveGraph` facades (the
native `Store` now covers named graphs), JSON-LD, streaming generators (F9),
GIL-release hot path (NF2), and the multi-version wheel matrix (F7/#9). See
`SPEC-10`, the design spec, and `TASKS.md`.

## Building the wheel

The crate is **deliberately excluded from the Cargo workspace** so the
hermetic `cargo build/test/clippy --workspace` never needs a Python
interpreter. Build the extension explicitly with maturin:

```bash
cd crates/python
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop --features extension-module   # builds + installs into the venv
pip install rdflib pytest                      # for the differential tests
pytest tests/
```

The pure-Rust core (term codec + `Graph` engine) is unit-tested without Python:

```bash
cargo test -p horndb-python   # from inside crates/python (own workspace)
```
