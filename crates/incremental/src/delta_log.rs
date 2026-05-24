//! Pending `(triple, ±1)` records between checkpoints. SPEC-06 F1 / F7.
//!
//! Stage 1 simplification: this is an in-memory `Vec`, not persisted. A
//! crash between checkpoints loses pending deltas — that matches SPEC-02
//! NF5 (Stage 1 crash recovery rolls back to last checkpoint). A
//! write-ahead-log version is a Stage 2 deliverable owned by SPEC-02.

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, TripleId};

#[derive(Debug, Default)]
pub struct DeltaLog {
    records: Vec<DeltaRecord>,
    next_time: LogicalTime,
}

impl DeltaLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn current_time(&self) -> LogicalTime {
        self.next_time
    }

    /// Append a record. Returns the logical time assigned to it.
    pub fn append(
        &mut self,
        triple: TripleId,
        mult: Multiplicity,
        kind: DerivationKind,
    ) -> LogicalTime {
        let time = self.next_time;
        self.next_time = self
            .next_time
            .checked_add(1)
            .expect("logical-time u64 overflow (~585 years at 1Gtps)");
        self.records.push(DeltaRecord {
            triple,
            mult,
            time,
            kind,
        });
        time
    }

    /// Borrow records in append order.
    pub fn iter(&self) -> impl Iterator<Item = &DeltaRecord> {
        self.records.iter()
    }

    /// Empty the log and return owned records, preserving order.
    pub fn drain(&mut self) -> impl Iterator<Item = DeltaRecord> + '_ {
        self.records.drain(..)
    }
}
