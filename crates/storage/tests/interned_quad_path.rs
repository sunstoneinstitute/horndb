//! The id-based write path (`Dictionary::intern_quad` + `Store::insert_quad_ids`)
//! must be indistinguishable from the term-based one (`Store::insert_quads`)
//! for the same document (HDB-87).
//!
//! Both paths intern exactly once per new term, in document order, so the two
//! stores must end with the same dictionary mapping and the same stored ids.
//! Without that the bulk loader's ids would drift from what the serial loader
//! assigns for the same input.
//!
//! Scope: this pins the two *paths* against each other. Since `apply_quads`
//! also routes through `intern_quad`, both sides share one interning routine,
//! so a change to the order *inside* `intern_triple` would move both sides
//! alike and still pass here. The independent order guard is
//! `parallel_loader.rs`, whose loader calls `intern_triple` directly.

use horndb_storage::{InternedQuad, Store, DEFAULT_GRAPH};
use oxrdf::{BlankNode, Literal, NamedNode, Term};

/// A document with repeated subjects/predicates/objects (so most interns are
/// hits), one duplicate triple, a plain literal, an inline-int literal and a
/// blank node.
fn corpus() -> Vec<(Term, Term, Term)> {
    let iri = |s: &str| Term::NamedNode(NamedNode::new(s).unwrap());
    let mut out = Vec::new();
    for i in 0..64 {
        let s = iri(&format!("http://example.org/s{}", i % 17));
        let p = iri(&format!("http://example.org/p{}", i % 5));
        out.push((
            s.clone(),
            p.clone(),
            iri(&format!("http://example.org/o{i}")),
        ));
        out.push((
            s.clone(),
            p.clone(),
            Term::Literal(Literal::new_simple_literal(format!("v{}", i % 7))),
        ));
        out.push((
            s,
            iri("http://example.org/count"),
            Term::Literal(Literal::new_typed_literal(
                (i % 3).to_string(),
                NamedNode::new("http://www.w3.org/2001/XMLSchema#integer").unwrap(),
            )),
        ));
    }
    out.push((
        Term::BlankNode(BlankNode::new("b0").unwrap()),
        iri("http://example.org/p0"),
        iri("http://example.org/o0"),
    ));
    // Exact duplicate of the first triple: both paths must absorb it the same way.
    out.push(out[0].clone());
    out
}

#[test]
fn insert_quad_ids_matches_insert_quads() {
    let doc = corpus();

    let term_path = Store::in_memory();
    let quads: Vec<_> = doc
        .iter()
        .map(|(s, p, o)| (DEFAULT_GRAPH, s.clone(), p.clone(), o.clone()))
        .collect();
    term_path.insert_quads(&quads).unwrap();

    let id_path = Store::in_memory();
    let interned: Vec<InternedQuad> = doc
        .iter()
        .map(|(s, p, o)| {
            id_path
                .dictionary()
                .intern_quad(DEFAULT_GRAPH, s, p, o)
                .unwrap()
        })
        .collect();
    id_path.insert_quad_ids(&interned).unwrap();

    // Same dictionary: same size, and every document term maps to the same id
    // and decodes back to itself on both sides.
    assert_eq!(id_path.dictionary().len(), term_path.dictionary().len());
    for (s, p, o) in &doc {
        for t in [s, p, o] {
            let a = id_path.dictionary().get(t).expect("term interned");
            let b = term_path.dictionary().get(t).expect("term interned");
            assert_eq!(a, b, "id drift for {t}");
            assert_eq!(id_path.dictionary().lookup(a).as_ref(), Some(t));
        }
    }

    // Same stored triples, as raw ids (key-ordered, so directly comparable).
    assert_eq!(id_path.scan_all_term_ids(), term_path.scan_all_term_ids());
    assert_eq!(id_path.triple_count(), term_path.triple_count());
}

#[test]
fn interned_quad_carries_the_ids_it_was_built_from() {
    let store = Store::in_memory();
    let d = store.dictionary();
    let s = Term::NamedNode(NamedNode::new("http://example.org/s").unwrap());
    let p = Term::NamedNode(NamedNode::new("http://example.org/p").unwrap());
    let o = Term::NamedNode(NamedNode::new("http://example.org/o").unwrap());
    let q = d.intern_quad(DEFAULT_GRAPH, &s, &p, &o).unwrap();
    assert_eq!(q.graph(), DEFAULT_GRAPH);
    assert_eq!(d.lookup(q.subject()).as_ref(), Some(&s));
    assert_eq!(d.lookup(q.predicate()).as_ref(), Some(&p));
    assert_eq!(d.lookup(q.object()).as_ref(), Some(&o));
    // Re-interning the same terms is a hit, not a new id.
    assert_eq!(q, d.intern_quad(DEFAULT_GRAPH, &s, &p, &o).unwrap());
}

/// The debug-only guard on the id-based entry point fires when a quad interned
/// against one store is handed to another. Release builds skip the check, so
/// the test does too.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "never issued")]
fn quad_from_another_stores_dictionary_trips_the_debug_guard() {
    let a = Store::in_memory();
    let b = Store::in_memory();
    let iri = |s: &str| Term::NamedNode(NamedNode::new(s).unwrap());
    let q = a
        .dictionary()
        .intern_quad(
            DEFAULT_GRAPH,
            &iri("http://example.org/s"),
            &iri("http://example.org/p"),
            &iri("http://example.org/o"),
        )
        .unwrap();
    // `b` has interned nothing, so none of those indices exist there.
    let _ = b.insert_quad_ids(&[q]);
}
