//! Engine-agnostic surface every reasoner implementation must satisfy.
//!
//! The harness uses only this trait; engines may be the in-tree
//! [`crate::stub::StubReasoner`] (SPEC-01 F12) or a real implementation
//! living in another workspace crate.

use anyhow::Result;
use oxrdf::Dataset;

/// A pluggable reasoning engine.
///
/// Contract:
/// * `load` is destructive — it replaces any previously-loaded data.
/// * `entails` and `is_consistent` must use the currently-loaded data.
/// * Implementations must be `Send + Sync` so the runner can hand them
///   to threaded backends in later stages without API churn.
pub trait Reasoner: Send + Sync {
    fn name(&self) -> &str;
    fn load(&mut self, dataset: &Dataset) -> Result<()>;
    fn entails(&self, conclusion: &Dataset) -> Result<bool>;
    fn is_consistent(&self) -> Result<bool>;
    fn ask(&self, query: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::marker::PhantomData;

    // Compile-time check: trait objects are Send + Sync.
    fn _assert_object_safe() {
        fn _f(_: &dyn Reasoner) {}
        fn _g<T: Send + Sync + ?Sized>(_: PhantomData<T>) {}
        _g::<dyn Reasoner>(PhantomData);
    }
}
