//! SPEC-02 #19 — copy-on-write snapshot isolation: concurrent readers see a
//! stable, consistent view while a single writer appends.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::sync::Arc;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn p() -> Term {
    iri("http://ex/p")
}

fn subj(i: u64) -> Term {
    iri(&format!("http://ex/s{i}"))
}

/// A reader that pins a snapshot sees a fixed triple count for the snapshot's
/// whole lifetime, regardless of how many triples the writer appends meanwhile.
#[test]
fn reader_pinned_snapshot_is_stable_under_concurrent_writes() {
    let store = Arc::new(Store::in_memory());

    // Seed 100 triples on predicate p (one batch -> version 1).
    let seed: Vec<(Term, Term, Term)> = (0..100)
        .map(|i| (subj(i), p(), iri("http://ex/o")))
        .collect();
    store.insert_triples(&seed).unwrap();

    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            // Append 1000 more triples, one batch at a time, to maximise the
            // chance of interleaving with the readers below.
            for i in 100..1100 {
                store
                    .insert_triples(&[(subj(i), p(), iri("http://ex/o"))])
                    .unwrap();
            }
        })
    };

    // Spawn readers that each pin a snapshot and repeatedly verify the count it
    // reports never changes for that snapshot's lifetime.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let store = Arc::clone(&store);
        readers.push(std::thread::spawn(move || {
            let snap = store.snapshot();
            let pinned_version = snap.version();
            let pinned_count = snap.triple_count();
            assert!(pinned_count >= 100, "snapshot must see at least the seed");
            for _ in 0..2000 {
                assert_eq!(
                    snap.triple_count(),
                    pinned_count,
                    "pinned snapshot triple count drifted under concurrent writes"
                );
                assert_eq!(snap.version(), pinned_version);
                // The materialized scan must match the count for the same view.
                let rows = snap.scan_predicate_default_graph(&p()).unwrap();
                assert_eq!(rows.len() as u64, pinned_count);
            }
        }));
    }

    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }

    // After all writes, a fresh snapshot sees everything.
    let final_snap = store.snapshot();
    assert_eq!(final_snap.triple_count(), 1100);
    // 1 seed batch + 1000 single-triple batches = version 1001.
    assert_eq!(final_snap.version(), 1001);
}

/// Two snapshots pinned at different times reflect their respective versions;
/// the older one is never disturbed by the writes that produced the newer one.
#[test]
fn older_snapshot_outlives_newer_writes() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(subj(0), p(), iri("http://ex/o"))])
        .unwrap();
    let early = store.snapshot();

    for i in 1..50 {
        store
            .insert_triples(&[(subj(i), p(), iri("http://ex/o"))])
            .unwrap();
    }
    let late = store.snapshot();

    assert_eq!(early.triple_count(), 1);
    assert_eq!(late.triple_count(), 50);
    assert!(late.version() > early.version());

    // Dropping the late snapshot does not affect the early one (no shared
    // mutable state); the early view is still its original size.
    drop(late);
    assert_eq!(early.triple_count(), 1);
    assert_eq!(early.scan_predicate_default_graph(&p()).unwrap().len(), 1);
}
