//! SPEC-25 S1 acceptance #1: deletes exist and snapshots stay honest.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::sync::Arc;
use std::thread;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn snapshot_export_excludes_retracted_triples() {
    let store = Store::in_memory();
    let a = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let c = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
    store.insert_triples(&[a.clone(), c.clone()]).unwrap();
    store.retract_triples(std::slice::from_ref(&a)).unwrap();

    let mut buf = Vec::new();
    let stats = store.export_snapshot(&mut buf).unwrap();
    assert_eq!(stats.triples, 1, "export sees only the live triple");

    // Round-trip into a fresh store: exactly the live triple.
    let restored = Store::in_memory();
    restored.import_snapshot(&mut buf.as_slice()).unwrap();
    assert_eq!(restored.triple_count(), 1);
}

#[test]
fn concurrent_reader_pinned_before_delete_is_stable() {
    let store = Arc::new(Store::in_memory());
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(std::slice::from_ref(&t)).unwrap();

    let pinned = store.snapshot(); // sees the triple
    let writer = {
        let store = store.clone();
        let t = t.clone();
        thread::spawn(move || {
            for _ in 0..100 {
                store.retract_triples(std::slice::from_ref(&t)).unwrap();
                store.insert_triples(std::slice::from_ref(&t)).unwrap();
            }
        })
    };
    // While the writer churns, the pinned view never changes.
    for _ in 0..1000 {
        assert_eq!(pinned.triple_count(), 1);
    }
    writer.join().unwrap();
}
