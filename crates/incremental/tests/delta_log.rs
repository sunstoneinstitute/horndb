use horndb_incremental::{DeltaLog, DerivationKind};

#[test]
fn new_log_is_empty_and_time_starts_at_zero() {
    let log = DeltaLog::new();
    assert_eq!(log.len(), 0);
    assert_eq!(log.current_time(), 0);
}

#[test]
fn append_returns_monotonic_times() {
    let mut log = DeltaLog::new();
    let t1 = log.append((1, 2, 3), 1, DerivationKind::Asserted);
    let t2 = log.append((4, 5, 6), 1, DerivationKind::Asserted);
    assert!(t2 > t1, "logical time must increase per append");
    assert_eq!(log.len(), 2);
}

#[test]
fn iter_returns_records_in_append_order() {
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((4, 5, 6), -1, DerivationKind::Asserted);
    let triples: Vec<_> = log.iter().map(|r| (r.triple, r.mult)).collect();
    assert_eq!(triples, vec![((1, 2, 3), 1), ((4, 5, 6), -1)]);
}

#[test]
fn drain_clears_the_log_and_returns_records() {
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    let drained: Vec<_> = log.drain().collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(log.len(), 0);
}
