//! horndb-closure — GraphBLAS-backed closure backend for SPEC-05.
//!
//! # Stage-1 surface
//!
//! Public API consumed by SPEC-04 (rule engine) and SPEC-02 (storage):
//!
//! - [`sink::ClosureBackend`] — trait the rule engine calls into to close
//!   `prp-trp`, `scm-sco`, `scm-spo`, and to push `owl:sameAs` unions.
//! - [`sink::TripleSink`] — trait the storage layer implements to receive
//!   inferred triples in bulk. The sink MUST tag these as
//!   "GraphBLAS-derived" so the rule engine does not re-fire on them
//!   (SPEC-05 F5).
//! - [`sink::BackendImpl`] / [`sink::default_backend`] — the concrete
//!   implementation we ship.
//! - [`sameas::EquivClasses`] — directly consultable by SPEC-04 and SPEC-07
//!   (SPARQL) for `owl:sameAs` resolution instead of scanning materialised
//!   `eq-*` triples.
//!
//! # Implementation notes
//!
//! - Boolean `(∨, ∧)` semiring closure via iterated [`grb::BoolMatrix::mxm_lor_land`].
//! - Per-predicate dense renumbering via [`dense_id::DenseIdMap`] (SPEC-05 F7);
//!   rebuilt from scratch at each bulk checkpoint (incremental invalidation
//!   is Stage 2).
//! - `owl:sameAs` is pure-Rust union-find (no GraphBLAS); canonical
//!   representative = min `DictId` in class.
//!
//! # Future work (NOT in Stage 1)
//!
//! - Incremental update (SPEC-05 F6): **insertion path implemented** via
//!   [`sink::IncrementalClosureBackend`] /
//!   [`closure::incremental::IncrementalTransitiveClosure`] — a single-edge
//!   insert updates only the affected slice (backward-reach × forward-reach)
//!   instead of re-closing. **Deletion/retraction is still Stage 2** (needs
//!   SPEC-06 DBSP deltas).
//! - GPU GraphBLAS backend: SPEC-09.
//! - LAGraph adoption: Stage 2 evaluation.
//! - Cost-aware closures via `(min, +)` semiring: not required by OWL 2 RL.
//! - Heuristic routing back to direct rule firing when `nnz(M_p) < 10⁴`:
//!   needs benchmark tuning, deferred to Stage 2 (see SPEC-05 risks).
//! - `GrB_Matrix_dup`-based fast clone in the wrapper; current code rebuilds
//!   via `extract_edges` + `from_edges` which is correct but extra-allocating.

pub mod closure;
pub mod dense_id;
pub mod error;
pub mod ffi;
pub mod grb;
pub mod sameas;
pub mod sink;
pub mod types;
