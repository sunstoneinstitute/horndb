//! Checkpoint merge: drain a `DeltaLog` into the base `Zset`. SPEC-06 F8.
//!
//! Stage 1: a single base `Zset<TripleId>` per circuit; merge is one
//! pass over the log in append order, summing into the base. Zero-row
//! pruning is delegated to `Zset::add`.
//!
//! Stage 2 deliverables (not here): persistent on-disk checkpoint
//! format (SPEC-02), tiered merge across hot/warm/cold (SPEC-02 F6),
//! incremental closure-matrix reconstruction (SPEC-05 F6).

use crate::delta_log::DeltaLog;
use crate::types::TripleId;
use crate::zset::Zset;

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
