//! The id-level reasoning load must produce exactly the closure the lexical
//! one did (HDB-117).
//!
//! `load_with_reasoning` used to hand the whole closure across the
//! owlrl/sparql boundary as `(String, String, String)` triples, which the
//! backend then re-parsed and re-interned term by term. It now passes engine
//! term ids plus the engine dictionary, and interns once per distinct term.
//! This pins that the two paths load the same triples — literals, language
//! tags and blank nodes included, since those are where the lexical
//! convention could differ.

#![cfg(feature = "reasoner")]

use horndb_sparql::exec::horn::{load_with_reasoning, HornBackend};
use oxrdf::{
    BlankNode, Dataset, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term as OxTerm,
};

fn nn(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A dataset that infers (`cax-sco` via the subclass chain) and carries every
/// term shape the lexical convention encodes differently: IRI, blank node,
/// plain literal, language-tagged literal, typed literal.
fn dataset() -> Dataset {
    let mut d = Dataset::default();
    let mut add = |s: NamedOrBlankNode, p: NamedNode, o: OxTerm| {
        d.insert(&Quad::new(s, p, o, GraphName::DefaultGraph));
    };
    let penguin = NamedOrBlankNode::NamedNode(nn("http://ex/Penguin"));
    add(penguin.clone(), nn(SUBCLASS), nn("http://ex/Bird").into());
    add(
        NamedOrBlankNode::NamedNode(nn("http://ex/Bird")),
        nn(SUBCLASS),
        nn("http://ex/Animal").into(),
    );
    add(
        NamedOrBlankNode::NamedNode(nn("http://ex/pingu")),
        nn(TYPE),
        nn("http://ex/Penguin").into(),
    );
    add(
        NamedOrBlankNode::NamedNode(nn("http://ex/pingu")),
        nn("http://ex/name"),
        Literal::new_simple_literal("Pingu").into(),
    );
    add(
        NamedOrBlankNode::NamedNode(nn("http://ex/pingu")),
        nn("http://ex/label"),
        Literal::new_language_tagged_literal("Pingu", "en")
            .unwrap()
            .into(),
    );
    add(
        NamedOrBlankNode::NamedNode(nn("http://ex/pingu")),
        nn("http://ex/age"),
        Literal::new_typed_literal("3", nn("http://www.w3.org/2001/XMLSchema#integer")).into(),
    );
    add(
        NamedOrBlankNode::BlankNode(BlankNode::new("b0").unwrap()),
        nn(TYPE),
        nn("http://ex/Penguin").into(),
    );
    d
}

fn sorted_text(b: &HornBackend) -> Vec<String> {
    let mut v: Vec<String> = b
        .iter_oxrdf()
        .iter()
        .map(|(s, p, o)| format!("{s} {p} {o}"))
        .collect();
    v.sort();
    v
}

#[test]
fn id_closure_matches_the_lexical_closure() {
    let data = dataset();

    // New path: ids across the boundary.
    let mut ids = HornBackend::new();
    let stats =
        load_with_reasoning(&mut ids, &data, horndb_owlrl::BackendChoice::RuleFiring).unwrap();

    // Old path: the same engine state, decoded to lexical triples and
    // re-parsed. Also the check that `materialized_triples` still works.
    let mut engine = horndb_owlrl::Engine::new();
    engine.load(&data).unwrap();
    let lexical = engine.materialized_triples().unwrap();
    let mut strings = HornBackend::new();
    let loaded = strings
        .load_lexical_triples(lexical.clone().into_iter())
        .unwrap();

    assert_eq!(
        stats.loaded, loaded,
        "both paths load the same triple count"
    );
    assert_eq!(stats.loaded as usize, lexical.len());
    assert_eq!(sorted_text(&ids), sorted_text(&strings));
    // The closure is bigger than the input: pingu is an Animal by two hops.
    assert!(sorted_text(&ids).iter().any(|t| {
        t
        == "<http://ex/pingu> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Animal>"
    }));
}
