//! Pending `(triple, ±1)` records between ticks, durable behind the storage
//! write-ahead log. SPEC-06 F1 / F7, SPEC-24 S5.
//!
//! A log with no store attached is an in-memory `Vec` (unit tests, benches):
//! a crash loses the pending records. [`DeltaLog::attach_wal`] backs it with
//! the store's log instead (ADR-0018 — one physical log, typed records):
//! every `append` writes an `Input` record, durable per the store's
//! `SyncPolicy`, and `Circuit::tick` writes a `TickCommit` marker for the
//! range it drained. Recovery re-submits the records past the last marker;
//! truncation happens when a checkpoint rolls the log's generation.

use std::sync::Arc;

use horndb_storage::{InputRecord, StorageError, Store};

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};

/// `DerivationKind` as the opaque `kind_tag` storage records. Asserted is 0
/// so the common case is a zero field; rule ids shift past the two
/// unparameterised kinds.
fn kind_tag(kind: DerivationKind) -> u64 {
    match kind {
        DerivationKind::Asserted => 0,
        DerivationKind::ClosureInferred => 1,
        DerivationKind::RuleInferred(rule) => 2 + rule as u64,
    }
}

fn next_after(time: LogicalTime) -> LogicalTime {
    time.checked_add(1)
        .expect("logical-time u64 overflow (~585 years at 1Gtps)")
}

fn kind_from_tag(tag: u64) -> DerivationKind {
    match tag {
        0 => DerivationKind::Asserted,
        1 => DerivationKind::ClosureInferred,
        t => DerivationKind::RuleInferred((t - 2) as RuleId),
    }
}

#[derive(Default)]
pub struct DeltaLog {
    records: Vec<DeltaRecord>,
    next_time: LogicalTime,
    /// The store whose write-ahead log makes `append` durable, if any.
    wal: Option<Arc<Store>>,
}

impl std::fmt::Debug for DeltaLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeltaLog")
            .field("records", &self.records)
            .field("next_time", &self.next_time)
            .field("wal_backed", &self.wal.is_some())
            .finish()
    }
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

    /// Make appends durable through `store`'s write-ahead log (ADR-0018).
    /// Errors on a store with no log — an in-memory store cannot make
    /// anything durable, and silently dropping the records would be worse.
    pub fn attach_wal(&mut self, store: Arc<Store>) -> Result<(), StorageError> {
        store.sync_wal()?;
        self.wal = Some(store);
        Ok(())
    }

    /// The store backing this log, if [`DeltaLog::attach_wal`] bound one.
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.wal.as_ref()
    }

    /// Append a record. Returns the logical time assigned to it. Durable
    /// before returning when the log is WAL-backed under the default
    /// `SyncPolicy::EveryBatch`.
    ///
    /// Panics if the durable append fails: the caller has no way to unwind a
    /// record the circuit is about to act on, and losing it silently would
    /// break the replay-to-identical-state contract.
    pub fn append(
        &mut self,
        triple: TripleId,
        mult: Multiplicity,
        kind: DerivationKind,
    ) -> LogicalTime {
        let time = self.next_time;
        if let Some(store) = &self.wal {
            store
                .log_input(&InputRecord {
                    seq: time,
                    triple,
                    mult,
                    kind_tag: kind_tag(kind),
                })
                .expect("delta-log input append");
        }
        self.push(DeltaRecord {
            triple,
            mult,
            time,
            kind,
        });
        time
    }

    /// Re-submit a record recovered from the log — already durable, so it is
    /// never appended again. Keeps the logical clock past every replayed
    /// time so later appends stay ordered.
    pub fn append_recovered(&mut self, rec: &InputRecord) {
        let replayed = DeltaRecord {
            triple: rec.triple,
            mult: rec.mult,
            time: rec.seq,
            kind: kind_from_tag(rec.kind_tag),
        };
        self.records.push(replayed);
        self.next_time = self.next_time.max(next_after(rec.seq));
    }

    fn push(&mut self, rec: DeltaRecord) {
        self.records.push(rec);
        self.next_time = next_after(self.next_time);
    }

    /// Record a tick boundary in the log: every input up to `last_seq` is
    /// drained (ADR-0018 `TickCommit`). Recovery replays each such batch as
    /// its own tick, so the derived state comes back with the tick grouping
    /// it had. A no-op on a log with no WAL backing.
    pub fn commit_tick(&mut self, last_seq: LogicalTime) {
        if let Some(store) = &self.wal {
            store
                .log_tick_commit(last_seq)
                .expect("delta-log tick marker");
        }
    }

    /// Borrow records in append order.
    pub fn iter(&self) -> impl Iterator<Item = &DeltaRecord> {
        self.records.iter()
    }

    /// Empty the log and return owned records, preserving order.
    pub fn drain(&mut self) -> impl Iterator<Item = DeltaRecord> + '_ {
        self.records.drain(..)
    }

    /// Move the pending records into a detached in-memory log, leaving this
    /// one empty and still WAL-backed. `Circuit::tick` merges the detached
    /// log into the base with [`crate::Checkpoint::merge`].
    pub fn take_pending(&mut self) -> DeltaLog {
        DeltaLog {
            records: std::mem::take(&mut self.records),
            next_time: self.next_time,
            wal: None,
        }
    }
}
