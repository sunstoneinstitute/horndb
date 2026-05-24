use reasoner_incremental::Zset;

#[test]
fn new_zset_is_empty() {
    let z: Zset<i64> = Zset::new();
    assert!(z.is_empty());
    assert_eq!(z.len(), 0);
}

#[test]
fn insert_then_get_returns_multiplicity() {
    let mut z = Zset::new();
    z.add(42, 1);
    assert_eq!(z.get(&42), 1);
    assert_eq!(z.len(), 1);
}

#[test]
fn adding_negative_cancels_positive() {
    let mut z = Zset::new();
    z.add(42, 1);
    z.add(42, -1);
    assert_eq!(z.get(&42), 0);
    assert!(z.is_empty(), "zero-multiplicity rows must be pruned");
}

#[test]
fn add_accumulates_multiplicities() {
    let mut z = Zset::new();
    z.add(42, 3);
    z.add(42, 2);
    assert_eq!(z.get(&42), 5);
}
