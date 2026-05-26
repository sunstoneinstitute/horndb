//! horndb-sparql — SPARQL 1.1 frontend.
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

/// Runtime configuration for the SPARQL pipeline.
///
/// Default is **SPARQL 1.1** semantics: triple-term patterns in
/// `TermPattern::Triple` are rejected at algebra-translation time so
/// callers expecting 1.1 behaviour don't silently get 1.2 results.
///
/// The flag is *runtime* (not a Cargo feature) so the HTTP server can
/// flip it per request (e.g. via `?rdf12=1` or an `Accept` extension)
/// without a rebuild. See SPEC-07 §"RDF 1.2 mode" / TASKS.md HIGH for
/// the migration plan.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub struct SparqlConfig {
    /// Allow RDF 1.2 triple terms in queries. Defaults to `false`.
    pub rdf12: bool,
}

impl SparqlConfig {
    /// Convenience: a config with RDF 1.2 triple-term semantics enabled.
    pub fn rdf12() -> Self {
        Self { rdf12: true }
    }
}
