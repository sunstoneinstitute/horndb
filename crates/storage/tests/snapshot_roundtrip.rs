//! SPEC-02 acceptance #5: HDT round-trip (import → store → export → re-import)
//! produces an isomorphic store under blank-node renaming.
//!
//! Our format preserves blank-node labels, so isomorphism reduces to exact
//! triple-set equality — we assert the stronger property.

use horndb_storage::Store;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use std::collections::BTreeSet;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// All default-graph triples as a comparable set of stringified terms.
fn triple_set(store: &Store) -> BTreeSet<(String, String, String)> {
    let dict = store.dictionary();
    store
        .scan_all_term_ids()
        .into_iter()
        .map(|(s, p, o)| {
            (
                dict.lookup(s).unwrap().to_string(),
                dict.lookup(p).unwrap().to_string(),
                dict.lookup(o).unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn round_trip_preserves_all_triples() {
    let store = Store::in_memory();
    store
        .insert_triples(&[
            (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b")),
            (iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c")),
            (
                iri("http://ex/a"),
                iri("http://ex/label"),
                Term::Literal(Literal::new_simple_literal("hello")),
            ),
            (
                iri("http://ex/a"),
                iri("http://ex/lang"),
                Term::Literal(Literal::new_language_tagged_literal("bonjour", "fr").unwrap()),
            ),
            (
                iri("http://ex/a"),
                iri("http://ex/age"),
                Term::Literal(Literal::new_typed_literal(
                    "42",
                    NamedNode::new("http://www.w3.org/2001/XMLSchema#integer").unwrap(),
                )),
            ),
            (
                Term::BlankNode(BlankNode::new("b0").unwrap()),
                iri("http://ex/p"),
                Term::BlankNode(BlankNode::new("b1").unwrap()),
            ),
        ])
        .unwrap();

    let before = triple_set(&store);

    let mut bytes = Vec::new();
    store.export_snapshot(&mut bytes).unwrap();

    let reimported = horndb_storage::import_snapshot(&mut &bytes[..]).unwrap();
    let after = triple_set(&reimported);

    assert_eq!(before, after, "round-trip lost or altered triples");
    assert_eq!(reimported.triple_count(), store.triple_count());
}

#[test]
fn empty_store_round_trips() {
    let store = Store::in_memory();
    let mut bytes = Vec::new();
    store.export_snapshot(&mut bytes).unwrap();
    let reimported = horndb_storage::import_snapshot(&mut &bytes[..]).unwrap();
    assert_eq!(reimported.triple_count(), 0);
}

#[test]
fn export_refuses_named_graph_data() {
    let store = Store::in_memory();
    let g = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
    store
        .insert_quads(&[(
            g,
            iri("http://ex/a"),
            iri("http://ex/p"),
            iri("http://ex/b"),
        )])
        .unwrap();
    let mut bytes = Vec::new();
    let err = store.export_snapshot(&mut bytes);
    assert!(err.is_err(), "expected named-graph guard to fire");
}
