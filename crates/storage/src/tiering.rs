//! Placement policy for the warm/cold partition seam (SPEC-25 S5).
//!
//! [`MemoryTier::rebalance`](crate::MemoryTier::rebalance) is the policy: one
//! call is one round. It reads the per-partition access counts collected since
//! the previous round and decides, per `(graph, predicate)` partition, whether
//! to demote it to the cold tier or promote it back.
//!
//! The trigger is explicit, like [`Store::compact`](crate::Store::compact) —
//! the caller (a server timer, a test, the harness) picks the cadence.

use crate::term::{GraphId, TermId};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Knobs for one rebalance round.
#[derive(Debug, Clone)]
pub struct TieringConfig {
    /// Where cold partition files are written. Normally
    /// [`Store::cold_dir`](crate::Store::cold_dir).
    pub cold_dir: PathBuf,
    /// How many consecutive rounds a warm partition must go unread before it
    /// is demoted. `0` and `1` both mean "demote on the first idle round".
    pub demote_after_idle_rounds: u32,
    /// Partitions with fewer visible rows than this are never demoted — the
    /// per-file overhead is not worth reclaiming.
    pub min_rows: usize,
}

impl TieringConfig {
    /// Defaults: demote after 2 idle rounds, never below 1024 rows.
    pub fn new(cold_dir: impl Into<PathBuf>) -> Self {
        Self {
            cold_dir: cold_dir.into(),
            demote_after_idle_rounds: 2,
            min_rows: 1024,
        }
    }
}

/// External bias on placement, applied *alongside* the access statistics and
/// never instead of them (SPEC-25 S5, `INTEGRATION-NOTES.md` F4).
///
/// Hints only ever add: a hinted partition is kept warm, or pulled warm ahead
/// of the stats. Nothing here can demote. An empty set therefore gives exactly
/// the placement a stats-only policy gives — which is the `ml.enabled = false`
/// contract, since a disabled advisor yields no hints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementHints {
    /// Partitions to keep (or make) warm this round.
    pub keep_warm: BTreeSet<(GraphId, TermId)>,
}

/// What one [`MemoryTier::rebalance`](crate::MemoryTier::rebalance) round did.
/// Both lists are in `(graph bits, predicate bits)` order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebalanceReport {
    pub demoted: Vec<(GraphId, TermId)>,
    pub promoted: Vec<(GraphId, TermId)>,
}

impl RebalanceReport {
    /// True if the round moved nothing.
    pub fn is_empty(&self) -> bool {
        self.demoted.is_empty() && self.promoted.is_empty()
    }
}
