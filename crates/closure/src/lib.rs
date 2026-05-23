//! reasoner-closure — GraphBLAS-backed closure backend for SPEC-05.
//!
//! Provides:
//! - Transitive-property closure via iterated Boolean MxM on SuiteSparse:GraphBLAS.
//! - `rdfs:subClassOf` and `rdfs:subPropertyOf` closures (same machinery).
//! - `owl:sameAs` equivalence classes via pure-Rust union-find.
//! - Per-predicate dense renumbering of dictionary IDs.
//! - Writeback into a `TripleSink` (implemented by the storage crate).

pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod types;
