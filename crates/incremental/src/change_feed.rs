//! Ordered MPMC stream of committed deltas. SPEC-06 F9, SPEC-24 S3.
//!
//! Design: each subscriber gets its own `crossbeam-channel` sender plus a
//! [`LagPolicy`], kept in a `RwLock<Vec<_>>`. Publish iterates senders and
//! drops any whose receiver was closed. Per-subscriber ordering is
//! guaranteed by the single publisher path through `Circuit`; this
//! type itself takes the publisher's word.
//!
//! Backpressure (SPEC-24 S3): [`subscribe_bounded`](ChangeFeed::subscribe_bounded)
//! gives the subscriber a bounded buffer so a slow consumer cannot grow
//! publisher memory without limit. What happens when that buffer fills is the
//! subscriber's own choice:
//!
//! - [`LagPolicy::DisconnectSlow`] (the default) drops the subscriber and
//!   counts it on `incremental_change_feed_dropped_subscribers`. The tick
//!   never stalls; the subscriber's receiver sees the channel close.
//! - [`LagPolicy::Block`] backpressures the publisher — `tick()` waits until
//!   the subscriber drains. Only for consumers that must not miss a record
//!   and whose latency the writer is willing to inherit.
//!
//! [`subscribe`](ChangeFeed::subscribe) stays unbounded and is the explicit
//! opt-out from both.

use std::sync::RwLock;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};

use crate::types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, TripleId};

pub type ChangeFeedRx = Receiver<DeltaRecord>;

/// What the feed does when a bounded subscriber's buffer is full.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LagPolicy {
    /// Backpressure the publisher: block the tick until the subscriber drains.
    Block,
    /// Drop the subscriber and count it. Default — a slow reader must not be
    /// able to stall writers.
    #[default]
    DisconnectSlow,
}

struct Subscriber {
    tx: Sender<DeltaRecord>,
    policy: LagPolicy,
}

#[derive(Default)]
pub struct ChangeFeed {
    subscribers: RwLock<Vec<Subscriber>>,
}

impl ChangeFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Unbounded subscriber: never dropped for lag, never blocks the tick, and
    /// grows without limit if the consumer falls behind. Prefer
    /// [`subscribe_bounded`](Self::subscribe_bounded).
    pub fn subscribe(&self) -> ChangeFeedRx {
        // An unbounded sender is never `Full`, so `DisconnectSlow` never fires.
        self.push_subscriber(unbounded(), LagPolicy::DisconnectSlow)
    }

    /// Bounded subscriber holding at most `capacity` undelivered records.
    /// `capacity` 0 is a rendezvous channel (meaningful only with
    /// [`LagPolicy::Block`]).
    pub fn subscribe_bounded(&self, capacity: usize, policy: LagPolicy) -> ChangeFeedRx {
        self.push_subscriber(bounded(capacity), policy)
    }

    fn push_subscriber(
        &self,
        (tx, rx): (Sender<DeltaRecord>, ChangeFeedRx),
        policy: LagPolicy,
    ) -> ChangeFeedRx {
        let count = {
            let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
            subs.push(Subscriber { tx, policy });
            subs.len()
        };
        horndb_metrics::metrics()
            .incremental
            .change_feed_subscribers
            .set(count as i64);
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
        let mut dropped_for_lag = 0u64;
        let count = {
            let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
            subs.retain(|s| match s.policy {
                LagPolicy::Block => s.tx.send(rec).is_ok(),
                LagPolicy::DisconnectSlow => match s.tx.try_send(rec) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => {
                        dropped_for_lag += 1;
                        false
                    }
                    Err(TrySendError::Disconnected(_)) => false,
                },
            });
            subs.len()
        };
        let m = horndb_metrics::metrics();
        m.incremental.change_feed_subscribers.set(count as i64);
        if dropped_for_lag > 0 {
            m.incremental
                .change_feed_dropped_subscribers
                .inc_by(dropped_for_lag);
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .read()
            .expect("change-feed lock poisoned")
            .len()
    }
}
