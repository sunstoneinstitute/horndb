//! Checkpoint merge: drain a `DeltaLog` into the base `Zset`. SPEC-06 F8.
//!
//! Stage 1: a single base `Zset<TripleId>` per circuit; merge is one
//! pass over the log in append order, summing into the base. Zero-row
//! pruning is delegated to `Zset::add`.
//!
//! [`CheckpointPolicy`] is the F8 cadence itself. `Circuit::tick` runs the
//! merge every tick and, on the cadence, drives the durable checkpoint:
//! persist the attached store and truncate the input log (SPEC-24 S5).
//!
//! Stage 2 deliverables (not here): tiered merge across hot/warm/cold
//! (SPEC-02 F6), incremental closure-matrix reconstruction (SPEC-05 F6).

use std::time::Duration;

use crate::delta_log::DeltaLog;
use crate::types::TripleId;
use crate::zset::Zset;

/// SPEC-06 F8 cadence: how much may accumulate before the circuit
/// checkpoints. Whichever limit is reached first fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPolicy {
    pub interval: Duration,
    /// Delta records (asserted plus derived) since the last checkpoint.
    pub deltas: usize,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
            deltas: 100_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckpointReport {
    pub merged: usize,
}

pub struct Checkpoint;

impl Checkpoint {
    pub fn merge(base: &mut Zset<TripleId>, log: &mut DeltaLog) -> CheckpointReport {
        let mut count = 0;
        for rec in log.drain() {
            base.add(rec.triple, rec.mult);
            count += 1;
        }
        CheckpointReport { merged: count }
    }
}
