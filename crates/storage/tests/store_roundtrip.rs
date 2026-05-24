use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

fn nn(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn insert_triple_and_query_by_predicate() {
    let store = Store::in_memory();
    let alice = nn("http://example.org/Alice");
    let bob = nn("http://example.org/Bob");
    let knows = nn("http://example.org/knows");
    store
        .insert_triples(&[
            (alice.clone(), knows.clone(), bob.clone()),
            (bob.clone(), knows.clone(), alice.clone()),
        ])
        .unwrap();
    assert_eq!(store.triple_count(), 2);

    let pairs = store.scan_predicate_default_graph(&knows).unwrap();
    let mut s_strings: Vec<String> = pairs.iter().map(|(s, _)| format!("{s}")).collect();
    s_strings.sort();
    assert_eq!(
        s_strings,
        vec![
            "<http://example.org/Alice>".to_string(),
            "<http://example.org/Bob>".to_string()
        ]
    );
}

#[test]
fn idempotent_insertion() {
    let store = Store::in_memory();
    let s = nn("http://example.org/a");
    let p = nn("http://example.org/p");
    let o = nn("http://example.org/b");
    store
        .insert_triples(&[(s.clone(), p.clone(), o.clone())])
        .unwrap();
    store.insert_triples(&[(s, p, o)]).unwrap();
    assert_eq!(store.triple_count(), 1);
}

#[test]
fn footprint_is_reported() {
    let store = Store::in_memory();
    let s = NamedNode::new("http://example.org/s").unwrap();
    let p = NamedNode::new("http://example.org/p").unwrap();
    let triples: Vec<_> = (0..1000u32)
        .map(|i| {
            (
                Term::NamedNode(s.clone()),
                Term::NamedNode(p.clone()),
                Term::NamedNode(NamedNode::new(format!("http://example.org/o{i}")).unwrap()),
            )
        })
        .collect();
    store.insert_triples(&triples).unwrap();
    let report = store.report_footprint();
    assert_eq!(report.triples, 1000);
    assert!(report.bytes_per_triple > 0.0);
    // 16 bytes (s/o columns) plus per-predicate overhead; sanity bound.
    assert!(
        report.bytes_per_triple < 64.0,
        "footprint {} bytes/triple exceeds Stage-1 sanity bound",
        report.bytes_per_triple
    );
}
