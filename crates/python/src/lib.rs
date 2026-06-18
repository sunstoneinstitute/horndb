//! horndb-python — an rdflib-compatible Python binding for HornDB (SPEC-10).
//!
//! See `docs/specs/SPEC-10-rdflib-compatible-python-api.md`. This crate is the
//! first SPEC-10 increment: the core graph-centric rdflib surface — term
//! classes, a `Graph` facade backed by the SPEC-07 store, Turtle/N-Triples
//! parse & serialise, and SPARQL query/update passthrough.
//!
//! The crate is intentionally OUTSIDE the workspace `members` list so the
//! hermetic `cargo build/test/clippy --workspace` never needs a Python
//! interpreter. Build the wheel with maturin (see this crate's CLAUDE.md).

pub mod graph;
pub mod term;

mod py;
