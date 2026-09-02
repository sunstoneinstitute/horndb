//! HDB-121: the dictionary reclaims terms no stored row mentions any more.
//!
//! The catalog workload is a continuous append + retract churn, so without a
//! sweep the dictionary grows for the life of the process however few triples
//! are live. These tests assert on the live-term counter and on stored key
//! bytes, which is the measurable form of "RSS plateaus" — a criterion bench
//! (`benches/dict_gc_churn.rs`) covers the cost, not the footprint.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

fn iri(s: String) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// One churn round: `n` fresh triples on a shared predicate, then retract them.
fn churn(store: &Store, round: usize, n: usize) {
    let rows: Vec<_> = (0..n)
        .map(|i| {
            (
                iri(format!("http://ex/s{round}-{i}")),
                iri("http://ex/p".to_string()),
                iri(format!("http://ex/o{round}-{i}")),
            )
        })
        .collect();
    store.insert_triples(&rows).unwrap();
    store.retract_triples(&rows).unwrap();
}

#[test]
fn churn_plateaus_live_terms_and_key_bytes() {
    let store = Store::in_memory();
    // One triple that is never retracted, so the store is not trivially empty.
    let keep = (
        iri("http://ex/keep-s".to_string()),
        iri("http://ex/p".to_string()),
        iri("http://ex/keep-o".to_string()),
    );
    store.insert_triples(std::slice::from_ref(&keep)).unwrap();

    let mut plateau = None;
    for round in 0..8 {
        churn(&store, round, 250);
        store.compact();

        let live = store.dictionary().live_len();
        let (key_bytes, keys) = store.dictionary().key_bytes();
        // 3 survivors: the kept subject and object, and the predicate (a
        // partition key even with no live row under it).
        assert_eq!(live, 3, "round {round}: live terms did not return to 3");
        assert_eq!(keys, 3, "round {round}: forward-map keys did not plateau");
        match plateau {
            None => plateau = Some(key_bytes),
            Some(b) => assert_eq!(b, key_bytes, "round {round}: key bytes grew"),
        }
        // Slots are consumed monotonically — ids are never re-issued.
        assert!(
            store.dictionary().len() >= (round + 1) * 500,
            "round {round}: index space is expected to keep growing"
        );
    }

    // The surviving triple is untouched by the sweep.
    assert_eq!(store.triple_count(), 1);
    assert!(store.dictionary().get(&keep.0).is_some());
    let id = store.dictionary().get(&keep.2).unwrap();
    assert_eq!(store.dictionary().lookup(id).as_ref(), Some(&keep.2));
}

#[test]
fn a_pinned_snapshot_keeps_its_terms_resolvable() {
    let store = Store::in_memory();
    let row = (
        iri("http://ex/s".to_string()),
        iri("http://ex/p".to_string()),
        iri("http://ex/o".to_string()),
    );
    store.insert_triples(std::slice::from_ref(&row)).unwrap();
    let pinned = store.snapshot();
    store.retract_triples(std::slice::from_ref(&row)).unwrap();

    store.compact();

    // The retraction is invisible to the pin, so its row survives compaction
    // and its terms survive the sweep.
    assert_eq!(pinned.len(), 1);
    let id = store.dictionary().get(&row.2).unwrap();
    assert_eq!(store.dictionary().lookup(id).as_ref(), Some(&row.2));

    // Once the pin is gone the row is reclaimable, and so are its terms.
    drop(pinned);
    store.compact();
    assert!(store.dictionary().get(&row.2).is_none());
}
