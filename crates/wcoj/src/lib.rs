//! horndb-wcoj — Leapfrog Triejoin query executor for RDF triple patterns.
//!
//! See `specs/SPEC-03-query-engine.md` for the full design. Ships the
//! leapfrog executor, a hash-join tree executor for hybrid plans, the
//! cost-based per-BGP planner (SPEC-23 §5.5), Arrow vectorization and
//! cancellation. Magic sets and SLG tabling are deferred.

pub mod batch;
pub mod cancel;
pub mod cardinality;
pub mod cost;
pub mod error;
pub mod estimator;
pub mod executor;
pub mod ids;
pub mod pattern;
pub mod plan;
pub mod planner;
pub mod source;
pub mod stats;
pub mod trie;

pub use error::WcojError;
pub use ids::{Ordering, TermId, Triple};
pub use pattern::{Bgp, Term, TriplePattern, Var};
