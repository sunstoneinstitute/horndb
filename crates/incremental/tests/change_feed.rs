use horndb_incremental::{ChangeFeed, DerivationKind};

#[test]
fn published_records_arrive_in_order() {
    let feed = ChangeFeed::new();
    let rx = feed.subscribe();

    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);
    feed.publish((4, 5, 6), 1, 1, DerivationKind::Asserted);
    feed.publish((7, 8, 9), -1, 2, DerivationKind::RuleInferred(42));

    let a = rx.recv().unwrap();
    let b = rx.recv().unwrap();
    let c = rx.recv().unwrap();

    assert_eq!(a.time, 0);
    assert_eq!(b.time, 1);
    assert_eq!(c.time, 2);
    assert_eq!(c.kind, DerivationKind::RuleInferred(42));
    assert_eq!(c.mult, -1);
}

#[test]
fn multiple_subscribers_each_see_all_records() {
    let feed = ChangeFeed::new();
    let rx1 = feed.subscribe();
    let rx2 = feed.subscribe();

    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);

    assert_eq!(rx1.recv().unwrap().triple, (1, 2, 3));
    assert_eq!(rx2.recv().unwrap().triple, (1, 2, 3));
}

#[test]
fn dropped_subscriber_does_not_block_publish() {
    let feed = ChangeFeed::new();
    let rx = feed.subscribe();
    drop(rx);
    // Must not panic / block.
    feed.publish((1, 2, 3), 1, 0, DerivationKind::Asserted);
}

// ---------------------------------------------------------------------------
// SPEC-24 S3 — bounded subscribers and lag policy.
//
// Every test here is deterministic: no sleeps, no timing. Progress is driven
// by the channel operations themselves — a blocking `recv` on the consumer
// side and a blocking `send` on the publisher side.
// ---------------------------------------------------------------------------

use horndb_incremental::{Circuit, DeltaRecord, LagPolicy};

/// The lag-drop counter is process-global, and `cargo test` runs this file's
/// tests as parallel threads in ONE process (only nextest isolates them). So a
/// before/after delta is not enough on its own: every test that can drop a
/// subscriber for lag must hold this lock while it measures.
static LAG_DROP_COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_lag_counter() -> std::sync::MutexGuard<'static, ()> {
    // A failing test poisons the lock; the counter itself is still fine.
    LAG_DROP_COUNTER.lock().unwrap_or_else(|e| e.into_inner())
}

fn dropped_subscribers() -> u64 {
    horndb_metrics::metrics()
        .incremental
        .change_feed_dropped_subscribers
        .get()
}

fn rec(s: u64) -> DeltaRecord {
    DeltaRecord {
        triple: (s, 0, 0),
        mult: 1,
        time: s,
        kind: DerivationKind::Asserted,
    }
}

/// `DisconnectSlow` (the default): a subscriber that stops reading is dropped
/// once its bounded buffer fills. The publisher never blocks, the subscriber's
/// buffer never grows past `capacity` (no unbounded memory), and the drop is
/// visible on both the gauge (via `subscriber_count`) and the drop counter.
#[test]
fn disconnect_slow_drops_the_lagging_subscriber_with_a_bounded_buffer() {
    let _guard = lock_lag_counter();
    let dropped_before = dropped_subscribers();
    let feed = ChangeFeed::new();
    let slow = feed.subscribe_bounded(2, LagPolicy::DisconnectSlow);
    assert_eq!(feed.subscriber_count(), 1);

    // Never read from `slow`. Records 0 and 1 fill the buffer; record 2 finds
    // it full and disconnects the subscriber. None of this blocks.
    for i in 0..5 {
        feed.publish_record(rec(i));
    }

    assert_eq!(
        feed.subscriber_count(),
        0,
        "the lagging subscriber must be dropped, and the gauge must say so"
    );
    assert_eq!(
        dropped_subscribers() - dropped_before,
        1,
        "the lag drop must be counted"
    );

    // Bounded memory: exactly `capacity` records were ever buffered for it.
    let buffered: Vec<_> = std::iter::from_fn(|| slow.try_recv().ok()).collect();
    assert_eq!(buffered.len(), 2, "buffer must never exceed capacity");
    assert!(slow.try_recv().is_err(), "sender side must be closed");
}

/// A dropped slow subscriber must not cost the well-behaved ones anything:
/// the fast subscriber still sees every record, once, in order.
#[test]
fn dropping_a_slow_subscriber_does_not_disturb_a_fast_one() {
    // Drops its slow subscriber for lag, so it moves the shared counter.
    let _guard = lock_lag_counter();
    let feed = ChangeFeed::new();
    let slow = feed.subscribe_bounded(1, LagPolicy::DisconnectSlow);
    let fast = feed.subscribe_bounded(64, LagPolicy::DisconnectSlow);

    for i in 0..10 {
        feed.publish_record(rec(i));
    }
    drop(slow);

    let got: Vec<u64> = std::iter::from_fn(|| fast.try_recv().ok())
        .map(|r| r.time)
        .collect();
    assert_eq!(got, (0..10).collect::<Vec<_>>());
    assert_eq!(feed.subscriber_count(), 1, "only the slow one is gone");
}

/// `Block`: the publisher backpressures instead of dropping. With a capacity-1
/// buffer and 1000 records, the publisher can only make progress as the
/// consumer drains — so this deadlocks if the send path is wrong — and the
/// consumer must see all 1000 records exactly once, in order.
#[test]
fn block_policy_backpressures_the_publisher_without_losing_records() {
    const N: u64 = 1000;
    let feed = std::sync::Arc::new(ChangeFeed::new());
    let rx = feed.subscribe_bounded(1, LagPolicy::Block);

    let publisher = {
        let feed = feed.clone();
        std::thread::spawn(move || {
            for i in 0..N {
                feed.publish_record(rec(i));
            }
        })
    };

    // `recv` blocks; the publisher's `send` blocks. Neither spins.
    let got: Vec<u64> = (0..N).map(|_| rx.recv().unwrap().time).collect();
    publisher.join().unwrap();

    assert_eq!(got, (0..N).collect::<Vec<_>>(), "no gaps, no duplicates");
    assert!(rx.try_recv().is_err(), "nothing extra was published");
}

/// End to end through the `Circuit`: a bounded subscriber that keeps up sees
/// every net record of every tick, once. This is the shape an engine consumer
/// (SPEC-24 S4) wires up.
#[test]
fn circuit_bounded_subscriber_receives_every_net_record() {
    let mut circuit = Circuit::new();
    let rx = circuit.subscribe_bounded(1024, LagPolicy::DisconnectSlow);

    for i in 0..50u64 {
        circuit.assert_triple((i, 1, 2));
    }
    let report = circuit.tick();

    let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(
        got.len(),
        report.asserted_merged + report.derived_merged,
        "a keeping-up bounded subscriber must lose nothing"
    );
    assert_eq!(circuit.subscriber_count(), 1, "it must not be dropped");
}

/// A `Block` subscriber parked mid-`send` must not hold the subscriber lock:
/// `subscriber_count()` and `subscribe()` on another thread must still return.
/// Regression test — sending inside `subscribers.write()` deadlocks a
/// tick-then-drain consumer on one thread and head-of-line blocks later
/// subscribers.
///
/// Deterministic by construction, no sleeps: the capacity-0 rendezvous channel
/// means the publisher's `send` CANNOT complete until this thread calls
/// `recv`, and this thread does not call `recv` until after the two lock-taking
/// calls have returned. With the send under the lock, those calls block
/// forever and the test hangs instead of passing. The rendezvous only orders
/// the publisher reaching the handover, not `publish_record` pushing the
/// record, so this test does not check what a late subscriber sees.
#[test]
fn a_blocked_publisher_does_not_hold_the_subscriber_lock() {
    let feed = std::sync::Arc::new(ChangeFeed::new());
    let rx = feed.subscribe_bounded(0, LagPolicy::Block);

    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let publisher = {
        let feed = feed.clone();
        std::thread::spawn(move || {
            // Hands over only once this thread is committed to publishing.
            entered_tx.send(()).unwrap();
            feed.publish_record(rec(7));
        })
    };
    entered_rx.recv().unwrap();

    // Both take the subscriber lock. Neither may wait on the parked publisher.
    assert!(feed.subscriber_count() >= 1);
    // Held to the end of the test: dropping the receiver here would make the
    // parked publisher reap it, an unrelated path for a lock-behaviour test.
    let _other = feed.subscribe();

    // Now let the publisher through.
    assert_eq!(rx.recv().unwrap().time, 7);
    publisher.join().unwrap();
}
