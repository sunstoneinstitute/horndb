use reasoner_incremental::{ChangeFeed, DerivationKind};

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
