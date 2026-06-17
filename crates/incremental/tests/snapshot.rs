//! SPEC-06 F7 — in-flight reader visibility (MVCC snapshots).

use horndb_incremental::Circuit;

const P: u64 = 100;

#[test]
fn empty_circuit_snapshot_is_empty_at_time_zero() {
    let circuit = Circuit::new();
    let snap = circuit.snapshot();
    assert!(snap.is_empty());
    assert_eq!(snap.len(), 0);
    assert_eq!(snap.logical_time(), 0);
}

#[test]
fn snapshot_sees_asserted_rows_after_tick() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();

    let snap = circuit.snapshot();
    assert_eq!(snap.len(), 2);
    assert!(snap.contains(&(1, P, 2)));
    assert!(snap.contains(&(2, P, 3)));
    assert!(!snap.contains(&(9, P, 9)));
    assert_eq!(snap.get(&(1, P, 2)), 1);
}
