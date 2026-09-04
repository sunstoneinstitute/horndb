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

    /// Fan `rec` out to every subscriber.
    ///
    /// **Sends happen outside the subscriber lock.** A `Block` subscriber's
    /// `send` waits for its consumer, and holding the lock across that wait
    /// would stall `subscribe` / `subscribe_bounded` / `subscriber_count` —
    /// a consumer that ticks and drains on one thread (the engine-wiring
    /// shape) would deadlock, and one `Block` subscriber would head-of-line
    /// block every later subscriber. So: snapshot the senders under a short
    /// read lock, send with no lock held, then re-acquire the write lock only
    /// if someone needs reaping.
    ///
    /// Per-subscriber ordering therefore relies on `publish_record` being
    /// called from ONE thread at a time — which the single publisher path
    /// through `Circuit::tick` guarantees. Concurrent publishers would
    /// interleave.
    pub fn publish_record(&self, rec: DeltaRecord) {
        let targets: Vec<(Sender<DeltaRecord>, LagPolicy)> = {
            let subs = self.subscribers.read().expect("change-feed lock poisoned");
            subs.iter().map(|s| (s.tx.clone(), s.policy)).collect()
        };

        let mut dropped_for_lag = 0u64;
        let mut failed: Vec<Sender<DeltaRecord>> = Vec::new();
        for (tx, policy) in targets {
            let delivered = match policy {
                LagPolicy::Block => tx.send(rec).is_ok(),
                LagPolicy::DisconnectSlow => match tx.try_send(rec) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => {
                        dropped_for_lag += 1;
                        false
                    }
                    Err(TrySendError::Disconnected(_)) => false,
                },
            };
            if !delivered {
                failed.push(tx);
            }
        }

        let count = if failed.is_empty() {
            // Nothing to reap — do not take the exclusive lock. A subscriber
            // added while we were sending is simply counted on the next
            // publish; this is a gauge, not a ledger.
            self.subscriber_count()
        } else {
            let mut subs = self.subscribers.write().expect("change-feed lock poisoned");
            // Identity by channel, not by index: the Vec may have grown while
            // the lock was released, and `same_channel` does not care.
            subs.retain(|s| !failed.iter().any(|f| s.tx.same_channel(f)));
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
