//! SPEC-02 acceptance #5: HDT round-trip (import → store → export → re-import)
//! produces an isomorphic store under blank-node renaming.
//!
//! Our format preserves blank-node labels, so isomorphism reduces to exact
//! triple-set equality — we assert the stronger property.
//!
//! SPEC-25 acceptance #4 extends that to quads: named graphs round-trip, a
//! Stage-1 snapshot still imports, and a version-bumped one is rejected by the
//! Stage-1 version gate.

use horndb_storage::{Store, DEFAULT_GRAPH};
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

/// Every quad, keyed by graph name ("" for the default graph), as a comparable
/// set of stringified terms.
fn quad_set(store: &Store) -> BTreeSet<(String, String, String, String)> {
    let snap = store.snapshot();
    let mut out = BTreeSet::new();
    for g in snap.graphs() {
        let name = if g == DEFAULT_GRAPH {
            String::new()
        } else {
            snap.graph_uri(g).unwrap().to_string()
        };
        for (s, p, o) in snap.scan_graph(g).unwrap() {
            out.insert((name.clone(), s.to_string(), p.to_string(), o.to_string()));
        }
    }
    out
}

/// SPEC-25 acceptance #4, clause 1: a store holding named-graph data exports
/// and re-imports to exact quad-set equality.
#[test]
fn round_trip_preserves_all_quads() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let g1 = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
    let g2 = store.intern_graph_uri(&iri("http://ex/graph2")).unwrap();
    store
        .insert_quads(&[
            (
                g1,
                iri("http://ex/a"),
                iri("http://ex/p"),
                iri("http://ex/b"),
            ),
            (
                g1,
                iri("http://ex/a"),
                iri("http://ex/q"),
                iri("http://ex/c"),
            ),
            (
                g1,
                Term::BlankNode(BlankNode::new("b0").unwrap()),
                iri("http://ex/p"),
                Term::Literal(Literal::new_language_tagged_literal("bonjour", "fr").unwrap()),
            ),
            // Same triple in a second graph: graph scoping must survive.
            (
                g2,
                iri("http://ex/a"),
                iri("http://ex/p"),
                iri("http://ex/b"),
            ),
            // g1's name reused as a plain term, to catch double-encoding.
            (
                g2,
                iri("http://ex/graph1"),
                iri("http://ex/p"),
                iri("http://ex/d"),
            ),
        ])
        .unwrap();

    let before = quad_set(&store);
    let mut bytes = Vec::new();
    let stats = store.export_snapshot(&mut bytes).unwrap();
    assert_eq!(stats.triples, 6, "stats must count every quad");

    let reimported = horndb_storage::import_snapshot(&mut &bytes[..]).unwrap();
    assert_eq!(
        before,
        quad_set(&reimported),
        "round-trip lost or moved quads"
    );
}

/// Clause 2: a Stage-1 (default-graph-only) snapshot still imports — and a
/// default-graph-only store still writes that Stage-1 version.
#[test]
fn default_graph_snapshot_stays_on_the_stage1_version() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let mut bytes = Vec::new();
    store.export_snapshot(&mut bytes).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        1,
        "a default-graph-only store must keep writing the Stage-1 layout"
    );
    let reimported = horndb_storage::import_snapshot(&mut &bytes[..]).unwrap();
    assert_eq!(quad_set(&store), quad_set(&reimported));
}

/// Clause 3: a snapshot holding named-graph data carries a bumped version, so
/// the Stage-1 reader gate (`version != 1` -> "unsupported snapshot version")
/// rejects it cleanly instead of misreading the graphs section.
#[test]
fn named_graph_snapshot_is_rejected_by_the_stage1_version_gate() {
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
    store.export_snapshot(&mut bytes).unwrap();

    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        2,
        "named-graph data must bump the format version"
    );

    // Run the reader with Stage-1's version ceiling: the same code path a
    // Stage-1 build takes, against a real version-bumped snapshot.
    let err = horndb_storage::snapshot::format::read_snapshot_upto(&mut &bytes[..], 1)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unsupported snapshot version 2"),
        "expected the Stage-1 version gate to fire, got: {err}"
    );

    // The current reader accepts it.
    horndb_storage::import_snapshot(&mut &bytes[..]).unwrap();
}
