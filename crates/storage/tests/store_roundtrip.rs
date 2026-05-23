use oxrdf::{NamedNode, Term};
use reasoner_storage::Store;

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
    let mut s_strings: Vec<String> = pairs
        .iter()
        .map(|(s, _)| format!("{}", s))
        .collect();
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
    store.insert_triples(&[(s.clone(), p.clone(), o.clone())]).unwrap();
    store.insert_triples(&[(s, p, o)]).unwrap();
    assert_eq!(store.triple_count(), 1);
}
