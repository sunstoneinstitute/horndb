use horndb_incremental::{Checkpoint, DeltaLog, DerivationKind, Zset};

#[test]
fn checkpoint_merges_pending_inserts_into_base() {
    let mut base: Zset<(u64, u64, u64)> = Zset::new();
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((4, 5, 6), 1, DerivationKind::Asserted);

    let report = Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(1, 2, 3)), 1);
    assert_eq!(base.get(&(4, 5, 6)), 1);
    assert_eq!(report.merged, 2);
    assert_eq!(log.len(), 0, "log must be drained after checkpoint");
}

#[test]
fn checkpoint_collapses_insert_then_retract_to_nothing() {
    let mut base: Zset<(u64, u64, u64)> = Zset::new();
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);
    log.append((1, 2, 3), -1, DerivationKind::Asserted);

    Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(1, 2, 3)), 0);
    assert_eq!(base.len(), 0, "no zero rows after checkpoint");
}

#[test]
fn checkpoint_preserves_existing_base_rows() {
    let mut base: Zset<(u64, u64, u64)> = Zset::from_iter([((7, 8, 9), 1)]);
    let mut log = DeltaLog::new();
    log.append((1, 2, 3), 1, DerivationKind::Asserted);

    Checkpoint::merge(&mut base, &mut log);

    assert_eq!(base.get(&(7, 8, 9)), 1);
    assert_eq!(base.get(&(1, 2, 3)), 1);
}
