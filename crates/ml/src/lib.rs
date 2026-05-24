//! reasoner-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod audit;
pub mod candidate;
pub mod config;
pub mod hotset;
pub mod planner;
pub mod provenance;
pub mod registry;
pub mod types;

pub use config::{MlConfig, MlConfigError};
pub use registry::MlRegistry;
