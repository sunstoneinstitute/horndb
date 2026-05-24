//! reasoner-sparql — SPARQL 1.1 frontend.
//!
//! See `specs/SPEC-07-sparql-frontend.md` for scope and acceptance
//! criteria. This crate provides:
//!
//! * a parser wrapping the `spargebra` crate,
//! * an internal algebra (a stable subset of `spargebra::algebra`),
//! * a planner producing `PhysicalPlan` trees,
//! * a runtime that drives a pluggable [`exec::Executor`] (SPEC-03),
//! * SPARQL JSON / CSV / TSV result serialisers,
//! * (with the `server` feature) an embedded `axum`-based HTTP
//!   endpoint exposing `/query` and `/update`.

pub mod algebra;
pub mod api;
pub mod error;
pub mod exec;
pub mod parser;
pub mod plan;
pub mod regime;
pub mod results;
pub mod update;

#[cfg(feature = "server")]
pub mod server;

pub use error::{Result, SparqlError};
