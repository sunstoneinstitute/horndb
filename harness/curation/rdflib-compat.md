# `rdflib-compat` conformance subset (SPEC-10)

The SPEC-10 acceptance criteria require a dedicated `rdflib-compat` subset
covering term classes, `Graph` mutation, namespace binding, parse/serialize,
and SPARQL query/update, **differential-tested against upstream `rdflib`**.

Unlike the OWL 2 / SPARQL 1.1 / RDF 1.2 suites, this one is not a
manifest-driven Rust suite (there is no RDF test manifest for "does our Python
API match rdflib's"). It is a **Python differential suite**: each case drives
both `horndb_rdflib` and `rdflib` through the same operation and asserts the
observable results agree (or, for documented divergences, that they differ in
exactly the specified way).

## Where it lives

- Suite: `crates/python/tests/test_rdflib_compat.py`
- Run by: the `python-rdflib-compat` job in `.github/workflows/ci.yml`
  (builds the wheel with maturin, installs `rdflib`, runs `pytest`).
- Locally:
  ```bash
  cd crates/python
  python -m venv .venv && . .venv/bin/activate
  pip install maturin rdflib pytest
  maturin develop --features extension-module
  pytest tests/
  ```

## Curated coverage (first increment)

| Area | SPEC-10 req | Cases |
|---|---|---|
| Term classes | F1 | `URIRef`/`BNode`/`Literal`/`Variable` equality, hashing, `str()`, concat, datatype/lang, fresh bnodes |
| Graph mutation | F2 | `add` (idempotent), `remove`, `__len__`, `__contains__`, iteration, `subjects`/`objects`/`value`, literal-subject rejection |
| Parse / serialize | F4 | Turtle + N-Triples parse; serialize round-trips through rdflib; unsupported-format error |
| SPARQL passthrough | F5 | SELECT bindings, ASK boolean, CONSTRUCT triple set, `INSERT DATA`/`DELETE DATA` |
| Namespaces | F6 | `Namespace` term access, `bind()` round-trip |

## Grading rules / intentional divergences

- **Hash values** need not equal rdflib's; only intra-library stability and
  usability as dict keys are required (rdflib hashes are version-specific).
- **`xsd:string` collapse**: a `Literal` with `datatype=xsd:string` reports
  `datatype is None`, matching rdflib's plain-literal convention.
- **Blank-node identity** is by label (HornDB's store is label-keyed); rdflib's
  Skolemization/freshness rules are not reproduced beyond "distinct labels
  differ, no-arg `BNode()` is unique".
- **Reasoning visibility**: graph-inspection APIs (`triples`, `len`,
  `__contains__`) are base-store only in this increment; entailment-aware
  answers are out of scope here (SPEC-10 NF4 / risk note).

## Deferred to later increments (Stage-2)

`Dataset`/`ConjunctiveGraph` named-graph differential cases (F3), TriG /
N-Quads / RDF/XML / JSON-LD formats, the full namespace-manager surface, and
the multi-version CPython build matrix on macOS + Linux (acceptance #7 beyond
the single Linux job). Tracked under issue #9.

**Streaming (F9).** `Graph.triples()` and the `subjects`/`predicates`/`objects`
projections return a materialised Python `list` in this increment — iterable
for the common `for t in g.triples(...)` idiom (tested), but not a lazy
generator, so `next(g.triples(...))` raises `TypeError` and a full scan
materialises every match. True lazy/streamed iteration over the lock-held store
(SPEC-10 F9 / NF2 GIL-release) is a Stage-2 increment; it touches the
GIL/lifetime model and is intentionally out of this first slice.
