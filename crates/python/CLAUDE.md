# `horndb-python` (SPEC-10) — agent notes

PyO3/maturin binding exposing an `rdflib`-compatible Python API over the Rust
engine. First SPEC-10 increment: core terms, `Graph`, parse/serialize (Turtle +
N-Triples), SPARQL query/update passthrough, namespaces.

## The one hard rule: keep the workspace Python-free

This crate is **deliberately NOT in the root `Cargo.toml` `members` list**. It
carries its own empty `[workspace]` table so cargo/maturin run from inside the
crate dir do not attach it to the root workspace. Consequence:

- `cargo build --workspace`, `cargo clippy --workspace --all-targets`, and
  `cargo test --workspace` **never compile this crate** and never need a Python
  interpreter or libpython. Verify after any change that those three commands
  behave exactly as before (the binding must not leak into the default path).
- CI runs the binding in a **separate** `python-rdflib-compat` job
  (`.github/workflows/ci.yml`) that installs Python + rdflib and builds the
  wheel with maturin. The main hermetic job is untouched.

## Layout

- `src/term.rs` — pure-Rust `RdfTerm` codec. Round-trips term *kind* through the
  SPEC-07 `MemStore`, whose lexical form is otherwise lossy (it reclassifies
  everything non-quoted as an IRI). IRIs are stored **bare** and literals in
  N-Triples quoted form to match the SPARQL write path
  (`sparql::update::subject_to_term`); blank nodes get a `_:` prefix so they
  re-read as blank rather than IRI. No PyO3 — unit-tested with plain `cargo test`.
- `src/graph.rs` — pure-Rust `RdfGraph` engine over `MemStore`: add/remove/
  triples/query/update, parse/serialize via `oxrdfio`. Also PyO3-free.
- `src/py.rs` — the thin PyO3 adapter: `URIRef`/`BNode`/`Literal`/`Variable`/
  `Namespace`/`Graph`/`Result`. The only part that needs Python to run.

## Build & test

```bash
# Pure-Rust core (no Python needed) — runs from inside this crate:
cargo test            # term codec + graph engine unit tests
cargo clippy --all-targets -- -D warnings

# Full extension + differential suite (needs Python + rdflib):
python -m venv .venv && . .venv/bin/activate
pip install maturin rdflib pytest
maturin develop --features extension-module
pytest tests/         # differential vs upstream rdflib (SPEC-10 acceptance #2/#6)
```

The `extension-module` feature is OFF by default so `cargo test` can link
libpython and run the Rust unit tests; maturin turns it on for the wheel (abi3,
one wheel for CPython 3.10+).

## Gotchas

- PyO3 0.23 generates the richcompare slot from `__eq__`/`__ne__`; do **not**
  also define `__richcmp__` (duplicate-slot compile error).
- `oxrdf::Subject` is deprecated → use `NamedOrBlankNode`; subject position has
  no triple-term variant in this oxrdf version.
- Keep `term.rs`/`graph.rs` free of `pyo3` so the core stays testable without an
  interpreter.

See `../../docs/specs/SPEC-10-rdflib-compatible-python-api.md` and
`harness/curation/rdflib-compat.md`.
