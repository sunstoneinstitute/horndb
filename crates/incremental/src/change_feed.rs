//! Ordered MPMC stream of committed deltas. SPEC-06 F9.
//!
//! Design: each subscriber gets its own unbounded `crossbeam-channel`
//! sender, kept in a `RwLock<Vec<_>>`. Publish iterates senders and
//! drops any whose receiver was closed. Per-subscriber ordering is
//! guaranteed by the single publisher path through `Circuit`; this
//! type itself takes the publisher's word.
//!
//! Stage-1 simplification: unbounded channels. A backpressure variant
//! (bounded + lag policy) is a Stage 2 deliverable.

use std::sync::RwLock;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, TripleId};

pub type ChangeFeedRx = Receiver<DeltaRecord>;

#[derive(Default)]
pub struct ChangeFeed {
    subscribers: RwLock<Vec<Sender<DeltaRecord>>>,
}

impl ChangeFeed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self) -> ChangeFeedRx {
        let (tx, rx) = unbounded();
        self.subscribers
            .write()
            .expect("change-feed lock poisoned")
            .push(tx);
        rx
    }

    pub fn publish(
        &self,
        triple: TripleId,
        mult: Multiplicity,
        time: LogicalTime,
        kind: DerivationKind,
    ) {
        self.publish_record(DeltaRecord {
            triple,
            mult,
            time,
            kind,
        });
    }

    pub fn publish_record(&self, rec: DeltaRecord) {
        let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
        subs.retain(|tx| tx.send(rec).is_ok());
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .read()
            .expect("change-feed lock poisoned")
            .len()
    }
}
