//! Storage tier abstraction.
//!
//! Stage 1 ships exactly one impl: `MemoryTier`. The trait exists so that
//! Stage 2/3 cold tiers (HDT, CXL, NVMe) can slot in behind the same
//! interface without touching call sites.

use crate::error::Result;
use crate::partition::Partition;
use crate::term::{GraphId, TermId};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct TierStats {
    pub graphs: u64,
    pub predicates: u64,
    pub triples: u64,
    pub bytes_estimated: u64,
}

/// Result of [`Tier::apply_quad_batch`]: how many quads were actually
/// retracted and inserted (SPEC-28 S6). Both counts are "actually changed"
/// counts, not "requested" counts — a quad that was already absent (for
/// `retracted`) or already visible after the deletions (for `inserted`) does
/// not add to either field.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct ApplyReport {
    pub retracted: usize,
    pub inserted: usize,
}

pub trait Tier: Send + Sync + std::any::Any {
    fn predicate(&self, graph: GraphId, predicate: TermId) -> Option<&Partition>;

    fn predicates(&self, graph: GraphId) -> Vec<TermId>;

    /// The graphs holding at least one visible quad. A graph whose every quad
    /// has been retracted is not returned — D11 (SPEC-28): a named graph
    /// exists iff it holds at least one visible quad, so a fully-retracted
    /// graph ceases to exist rather than lingering as an empty entry.
    /// Includes `DEFAULT_GRAPH` when it holds data.
    fn graphs(&self) -> Vec<GraphId>;

    fn triple_count(&self) -> u64;

    fn stats(&self) -> TierStats;

    fn as_any(&self) -> &dyn std::any::Any;
}

/// The write half of a tier. `Store::tier()` hands out `&dyn Tier` only, so
/// every write from outside the storage crate goes through a `Store` entry
/// point and its write-ahead log (SPEC-25 S3); a bare `MemoryTier` in a test
/// or bench is the one place this trait is called directly.
pub trait TierWrite: Tier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()>;

    /// Retract a batch of quads. Stamps each matching live tuple's `end` at the
    /// new commit version (one batch = one version). Retracting an absent or
    /// already-dead tuple is a counted no-op, not an error. Returns the number
    /// of tuples actually retracted.
    fn retract_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<usize>;

    /// Apply a combined batch of deletions and insertions as a single commit
    /// version (SPEC-28 S6, the store boundary every named-graph update
    /// funnels through). Deletions apply before insertions, so a delete+insert
    /// of the same quad within one batch ends present. `retracted` counts only
    /// quads that were actually visible before the deletions; `inserted`
    /// counts only quads not visible *after* the deletions — so a quad deleted
    /// and re-added within one batch counts once retracted, once inserted.
    /// Quad identity is lexical term equality (no value normalization). A
    /// batch whose net effect is empty (nothing actually retracted or
    /// inserted) does not bump the version.
    fn apply_quad_batch(
        &self,
        dels: &[(GraphId, TermId, TermId, TermId)],
        adds: &[(GraphId, TermId, TermId, TermId)],
    ) -> Result<ApplyReport>;
}
