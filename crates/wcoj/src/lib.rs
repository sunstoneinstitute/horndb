//! reasoner-wcoj — Leapfrog Triejoin query executor for RDF triple patterns.
//!
//! See `specs/SPEC-03-query-engine.md` for the full design. Stage 0/1 scope:
//! WCOJ on ≥4 patterns, binary-hash-join fallback, Arrow vectorization,
//! cancellation. Magic sets and SLG tabling are deferred.

pub mod batch;
pub mod cancel;
pub mod cardinality;
pub mod error;
pub mod executor;
pub mod ids;
pub mod pattern;
pub mod plan;
pub mod planner;
pub mod source;
pub mod trie;

pub use error::WcojError;
pub use ids::{Ordering, TermId, Triple};
pub use pattern::{Bgp, Term, TriplePattern, Var};
