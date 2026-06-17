//! SPEC-06 F7 — in-flight reader visibility (MVCC snapshots).

use horndb_incremental::{Circuit, TransitiveClosureRule};

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

#[test]
fn snapshot_is_pinned_across_a_later_tick() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();

    let snap = circuit.snapshot();
    assert_eq!(snap.len(), 1);

    // A later tick adds a new triple. The pinned snapshot must NOT see it.
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    assert_eq!(snap.len(), 1, "snapshot must stay pinned across the tick");
    assert!(snap.contains(&(1, P, 2)));
    assert!(!snap.contains(&(3, P, 4)));

    // A fresh snapshot does see both.
    let fresh = circuit.snapshot();
    assert_eq!(fresh.len(), 2);
    assert!(fresh.contains(&(3, P, 4)));
}

#[test]
fn overlapping_snapshots_stay_independent() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();
    let s1 = circuit.snapshot();

    circuit.assert_triple((2, P, 3));
    circuit.tick();
    let s2 = circuit.snapshot();

    circuit.assert_triple((3, P, 4));
    circuit.tick();
    let s3 = circuit.snapshot();

    assert_eq!(s1.len(), 1, "s1 pinned at 1 triple");
    assert_eq!(s2.len(), 2, "s2 pinned at 2 triples");
    assert_eq!(s3.len(), 3, "s3 sees all 3");

    // Logical time advances across ticks that merge asserted records.
    assert!(s1.logical_time() < s2.logical_time());
    assert!(s2.logical_time() < s3.logical_time());
}

#[test]
fn snapshot_includes_and_pins_derived_rows() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // 1->2, 2->3 ⇒ transitive closure derives 1->3.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();

    let snap = circuit.snapshot();
    assert!(snap.contains(&(1, P, 2)), "asserted edge");
    assert!(snap.contains(&(2, P, 3)), "asserted edge");
    assert!(
        snap.contains(&(1, P, 3)),
        "derived transitive edge in snapshot"
    );
    let pinned_len = snap.len();

    // Extend the chain; the derived 1->4/2->4/3->4 etc. must not leak into the
    // pinned snapshot.
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    assert_eq!(snap.len(), pinned_len, "derived rows stay pinned");
    assert!(
        !snap.contains(&(1, P, 4)),
        "new derived edge absent from old snap"
    );

    let fresh = circuit.snapshot();
    assert!(
        fresh.contains(&(1, P, 4)),
        "fresh snapshot sees new derived edge"
    );
}

use std::sync::mpsc;
use std::thread;

// NF4: readers do not block writers and writers do not block readers. A
// snapshot is Send + Sync (Arc-backed), so a reader thread can poll it
// concurrently with a writer thread driving ticks; the pinned view stays
// constant for the snapshot's whole lifetime.
#[test]
fn reader_does_not_block_writer_and_view_stays_stable() {
    let mut circuit = Circuit::new();
    circuit.assert_triple((1, P, 2));
    circuit.tick();

    let snap = circuit.snapshot();
    let baseline = snap.len();
    let (tx, rx) = mpsc::channel();

    let reader = thread::spawn(move || {
        // Poll the pinned snapshot many times; it must never change.
        let mut observed = Vec::new();
        for _ in 0..10_000 {
            observed.push(snap.len());
        }
        tx.send(()).unwrap();
        observed
    });

    // Writer keeps ticking while the reader polls — must not block.
    for i in 0..2_000u64 {
        circuit.assert_triple((i + 10, P, i + 11));
        circuit.tick();
    }
    rx.recv().unwrap();
    let observed = reader.join().unwrap();

    assert!(
        observed.iter().all(|&n| n == baseline),
        "pinned snapshot len must stay constant under concurrent writes"
    );
    // The writer made progress concurrently.
    assert!(circuit.snapshot().len() > baseline);
}

// The snapshot is a *presence* union `asserted ∪ derived`: a triple present in
// both bases (derived, then asserted by the user) or asserted more than once is
// exposed exactly once at multiplicity 1 — not summed.
#[test]
fn snapshot_is_presence_union_not_multiplicity_sum() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Derive (1,P,3) via the transitive closure of 1->2->3.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();
    assert_eq!(circuit.snapshot().get(&(1, P, 3)), 1, "derived once");

    // Now the user *also* asserts the already-derived triple, and double-asserts
    // a fresh one. Neither must inflate the snapshot multiplicity past 1.
    circuit.assert_triple((1, P, 3)); // overlaps the derived row
    circuit.assert_triple((9, P, 9));
    circuit.assert_triple((9, P, 9)); // duplicate assertion
    circuit.tick();

    let snap = circuit.snapshot();
    assert_eq!(
        snap.get(&(1, P, 3)),
        1,
        "asserted∩derived stays multiplicity 1"
    );
    assert_eq!(
        snap.get(&(9, P, 9)),
        1,
        "double-asserted stays multiplicity 1"
    );
    // Every row in the view is presence (multiplicity exactly 1).
    assert!(
        snap.iter().all(|(_, m)| m == 1),
        "presence union: every triple at multiplicity 1"
    );
}
