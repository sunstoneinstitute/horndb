//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod audit;
pub mod candidate;
pub mod config;
pub mod hotset;
pub mod nlquery;
pub mod planner;
pub mod provenance;
pub mod registry;
pub mod types;

/// HTTP boundary (SPEC-08 F3 `/nl-query`, F6 `/ml-audit`). Behind the
/// `server` feature so the in-process traits stay axum/tokio-free.
#[cfg(feature = "server")]
pub mod server;

pub use config::{LlmPrivacy, MlConfig, MlConfigError};
pub use nlquery::{
    CostReport, NlQuestion, SparqlExecutor, TranslateError, Translation, Translator,
};
pub use registry::MlRegistry;
