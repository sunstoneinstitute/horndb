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

/// How the no-dataset default graph is composed (SPEC-28 S3, decision D2).
///
/// Applies only when a query has **no** `FROM`/`FROM NAMED` clause — an
/// explicit `FROM` list always wins (SPEC-28 S3). Reserved graphs (the
/// `https://horndb.io/graph/` prefix) are excluded from the union in both
/// modes.
///
/// This enum is a plan-and-config-level marker only as of PLAN-28-03 Task 2:
/// it is threaded from config through [`SparqlConfig`] to the HTTP layer, but
/// the executor does not yet consult it (that lands in PLAN-28-03 Task 3).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DefaultGraphMode {
    /// The union of every non-reserved graph — the SPARQL-friendly default.
    Union,
    /// Only the default-graph sentinel; no named-graph data is visible.
    Strict,
}

impl Default for DefaultGraphMode {
    fn default() -> Self {
        Self::Union
    }
}

impl DefaultGraphMode {
    /// Parse the `[server.limits].default_graph` config value / the
    /// `default-graph` per-query URL override. `None` for anything but
    /// exactly `"union"` or `"strict"` — callers name the offending value in
    /// their own error (config: startup-fatal in `serve.rs`; per-query: a
    /// 400 naming the `default-graph` key).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "union" => Some(Self::Union),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// Runtime configuration for the SPARQL pipeline.
///
/// Default is **SPARQL 1.1** semantics: triple-term patterns in
/// `TermPattern::Triple` are rejected at algebra-translation time so
/// callers expecting 1.1 behaviour don't silently get 1.2 results.
///
/// The flags are *runtime* (not a Cargo feature) so the HTTP server can
/// flip them per request (e.g. via `?rdf12=1` or `?default-graph=strict`)
/// without a rebuild. See SPEC-07 §"RDF 1.2 mode" / TASKS.md HIGH for
/// the migration plan, and SPEC-28 S3/D2 for `default_graph`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub struct SparqlConfig {
    /// Allow RDF 1.2 triple terms in queries. Defaults to `false`.
    pub rdf12: bool,
    /// How the no-dataset default graph is composed. Defaults to `Union`.
    pub default_graph: DefaultGraphMode,
}

impl SparqlConfig {
    /// Convenience: a config with RDF 1.2 triple-term semantics enabled.
    pub fn rdf12() -> Self {
        Self {
            rdf12: true,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_graph_mode_defaults_to_union() {
        assert_eq!(DefaultGraphMode::default(), DefaultGraphMode::Union);
        assert_eq!(
            SparqlConfig::default().default_graph,
            DefaultGraphMode::Union
        );
    }

    #[test]
    fn default_graph_mode_parses_known_values() {
        assert_eq!(
            DefaultGraphMode::parse("union"),
            Some(DefaultGraphMode::Union)
        );
        assert_eq!(
            DefaultGraphMode::parse("strict"),
            Some(DefaultGraphMode::Strict)
        );
    }

    #[test]
    fn default_graph_mode_rejects_unknown_values() {
        assert_eq!(DefaultGraphMode::parse("bogus"), None);
        assert_eq!(DefaultGraphMode::parse("Union"), None); // case-sensitive
        assert_eq!(DefaultGraphMode::parse(""), None);
    }
}
