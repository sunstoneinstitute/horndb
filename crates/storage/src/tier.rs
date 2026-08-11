//! Storage tier abstraction.
//!
//! Stage 1 ships exactly one impl: `MemoryTier`. The trait exists so that
//! Stage 2/3 cold tiers (HDT, CXL, NVMe) can slot in behind the same
//! interface without touching call sites.

use crate::error::Result;
use crate::partition::PredicatePartition;
use crate::term::{GraphId, TermId};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct TierStats {
    pub graphs: u64,
    pub predicates: u64,
    pub triples: u64,
    pub bytes_estimated: u64,
}

/// The result of one [`Tier::apply_quad_batch`] commit (SPEC-28 S6).
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct ApplyReport {
    /// Quads visible before the batch that this batch ends. Counts only
    /// quads actually visible beforehand — retracting an absent or
    /// already-dead quad is not counted.
    pub retracted: usize,
    /// Quads not visible before the batch (after the dels) that this batch
    /// makes live. A quad deleted and re-added within the same batch counts
    /// once retracted, once inserted; a re-insert of an already-visible quad
    /// counts neither.
    pub inserted: usize,
}

pub trait Tier: Send + Sync + std::any::Any {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()>;

    /// Retract a batch of quads. Stamps each matching live tuple's `end` at the
    /// new commit version (one batch = one version). Retracting an absent or
    /// already-dead tuple is a counted no-op, not an error. Returns the number
    /// of tuples actually retracted.
    fn retract_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<usize>;

    /// Atomically apply a batch of retractions and insertions as a single
    /// commit version (SPEC-28 S6): `dels` take effect before `adds`, so a
    /// del+add of the same quad within one batch ends the batch with that
    /// quad present. See [`ApplyReport`] for the count semantics. A batch
    /// whose net effect is empty (nothing actually retracted or inserted)
    /// does not bump the version — the combined-path extension of
    /// `retract_quad_batch`'s existing no-op-no-bump behaviour.
    fn apply_quad_batch(
        &self,
        dels: &[(GraphId, TermId, TermId, TermId)],
        adds: &[(GraphId, TermId, TermId, TermId)],
    ) -> Result<ApplyReport>;

    fn predicate(&self, graph: GraphId, predicate: TermId) -> Option<&PredicatePartition>;

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
