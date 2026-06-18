# horndb-rdflib

An [`rdflib`](https://rdflib.readthedocs.io/)-compatible Python API for
[HornDB](https://github.com/sunstoneinstitute/horndb), implemented as a
PyO3/maturin binding over the Rust engine. See
[`SPEC-10`](../../docs/specs/SPEC-10-rdflib-compatible-python-api.md) for the
contract.

This is the **first SPEC-10 increment**: the core graph-centric surface.

## What works today

```python
from horndb_rdflib import Graph, URIRef, Literal, Namespace

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

`Dataset` / `ConjunctiveGraph` named graphs (F3), formats beyond Turtle/NT
(TriG, N-Quads, RDF/XML, JSON-LD), the full namespace-manager surface,
streaming generators (F9), GIL-release hot path (NF2), and the drop-in `rdflib`
import name (F8 packaging decision). See `SPEC-10` and issue #9.

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
