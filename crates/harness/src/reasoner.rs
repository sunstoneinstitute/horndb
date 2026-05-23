//! Engine-agnostic surface that every reasoner implementation must
//! provide so the harness can run W3C tests against it.
//!
//! Filled out in Task 4.

use anyhow::Result;
use oxrdf::Dataset;

/// A pluggable reasoning engine.
///
/// The harness uses only this trait; engines may be the in-tree
/// [`crate::stub::StubReasoner`] or a real implementation living in
/// another workspace crate.
pub trait Reasoner: Send + Sync {
    /// Human-readable name (used in result-DB rows and reports).
    fn name(&self) -> &str;

    /// Load a dataset of ground triples into the reasoner. Replaces any
    /// previously-loaded data.
    fn load(&mut self, dataset: &Dataset) -> Result<()>;

    /// Check whether `conclusion` is OWL 2 RL entailed by the currently
    /// loaded dataset.
    fn entails(&self, conclusion: &Dataset) -> Result<bool>;

    /// Whether the currently loaded dataset is consistent.
    fn is_consistent(&self) -> Result<bool>;

    /// Evaluate a SPARQL 1.1 ASK query. Returns the boolean answer.
    fn ask(&self, query: &str) -> Result<bool>;
}
