# Plan — native pyoxigraph-shaped Python `Store` (SPEC-10 extension)

Implementation log for
[`docs/specs/2026-06-20-pyoxigraph-style-python-store.md`]. Delivers the native
`horndb` quad-store API alongside the existing rdflib facade (relocated to
`horndb.rdflib`).

## Shape

```
crates/python/src/
  term.rs        # (unchanged) kind-preserving RdfTerm codec
  graph.rs       # (unchanged behaviour) rdflib Graph engine; a few helpers made pub(crate)
  quadstore.rs   # NEW — pure-Rust QuadStore: named graphs, quads_for_pattern,
                 #       load/serialize, query(union)/update, materialize(). No PyO3.
  py.rs          # rdflib PyO3 classes; module renamed horndb_rdflib -> horndb;
                 #       rdflib classes registered under the horndb.rdflib submodule
  store_py.rs    # NEW — native PyO3 classes: NamedNode/BlankNode/Literal/Triple/
                 #       Quad/DefaultGraph/Variable/RdfFormat/QuerySolutions/
                 #       QuerySolution/Store
```

## Steps (done)

1. **Reuse seam.** Made `graph.rs::{alg_term_to_rdfterm, rdfterm_to_oxterm,
   subject_to_rdfterm, object_to_rdfterm}` `pub(crate)` so the quad store shares
   the term codec and SPARQL-answer conversion with the rdflib engine.
2. **`quadstore.rs`.** `GraphName` (Default/Named/Blank), `IoFormat`
   (Turtle/NTriples/NQuads/TriG/RdfXml), `QuadStore` with a dedup index over
   `(s, p, o, graph)` lexical keys. Query bridges to the SPEC-07 `MemStore`:
   build the active triple set (default graph, or union of all graphs) then call
   `execute_query`. Update runs `execute_update` against a default-graph
   `MemStore` and rebuilds the default graph. `materialize()` builds an
   `oxrdf::Dataset` from the asserted quads, runs
   `horndb_owlrl::integration::Engine`, and inserts the closure-minus-asserted
   triples into the default graph as `inferred`. 12 unit tests (`cargo test`).
3. **`store_py.rs`.** Thin PyO3 adapter over `QuadStore`; pyoxigraph-shaped
   value objects and result types; accepts `Quad` objects or `(s,p,o[,g])`
   tuples; `RdfFormat` class-constants or format strings.
4. **Module packaging.** `[lib] name` and maturin `module-name` →  `horndb`.
   `py.rs` `#[pymodule] fn horndb` registers the native classes at top level and
   builds the `horndb.rdflib` submodule (with a `sys.modules` insert so
   `import horndb.rdflib` resolves).
5. **Deps.** Added `horndb-owlrl` (path) — pure Rust, RuleFiring backend, so the
   wheel stays GraphBLAS-free. Confirmed `reasoner` does not pull
   `horndb-closure`.
6. **Tests.** `tests/test_native_store.py` (native surface + the exact
   `rdf-registry` discovery query + optional pyoxigraph differential);
   `tests/test_rdflib_compat.py` import updated to `horndb.rdflib`.
7. **CI.** `python-rdflib-compat` job installs `pyoxigraph` too and runs the
   whole `tests/` dir.
8. **Docs.** This plan, the design spec, `docs/rdflib.md`, `architecture.md`
   §12, the crate README + CLAUDE.md, `docs/index.md`, and the `TASKS.md` entry.

## Verification

- `cargo test` (in `crates/python`): 32 pass (20 rdflib + 12 quadstore),
  including OWL-RL materialize, union-vs-default query, named-graph load/
  serialize round-trips, `quads_for_pattern` graph filtering.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo build --lib`: the PyO3 cdylib compiles against libpython.
- The maturin wheel build + pytest run is CI-only (no maturin/pyoxigraph in the
  dev sandbox); the pure-Rust core gives local coverage.

## Follow-ups (see `TASKS.md`)

- Graph-scoped SPARQL (`GRAPH`/`FROM`/`FROM NAMED`) once the engine grows
  named-graph evaluation; then `query` can drop the binary union knob for real
  dataset semantics.
- DataFrame results (`.to_polars()`) + DataFrame→RDF mapping (maplib) for the
  sunstone-py integration.
- Per-graph indexing in `QuadStore` (today `quads_for_pattern` is a linear scan
  — fine for registry-scale data, not for large stores).
- Named-graph SPARQL Update; preserve inferred triples across updates.
