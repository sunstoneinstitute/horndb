//! HDB-352 — the bundled `skos-core` schema: hierarchy entailments fire, and
//! class hierarchies do not leak into the SKOS concept hierarchy.
use horndb_owlrl::schemas::skos_core;
use horndb_owlrl::Engine;
use std::collections::HashSet;

const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const EX: &str = "https://ex.org/";

type T = (String, String, String);

fn t(s: &str, p: &str, o: &str) -> T {
    (s.to_string(), p.to_string(), o.to_string())
}
fn skos(local: &str) -> String {
    format!("{SKOS}{local}")
}
fn ex(local: &str) -> String {
    format!("{EX}{local}")
}

fn close(data: Vec<T>) -> HashSet<T> {
    let mut e = Engine::new();
    e.load_base(skos_core().into_iter().chain(data)).unwrap();
    e.materialized_triples().unwrap().into_iter().collect()
}

#[test]
fn hierarchy_entailments_fire() {
    let c = close(vec![
        t(&ex("a"), &skos("broader"), &ex("b")),
        t(&ex("b"), &skos("broader"), &ex("c")),
        t(&ex("a"), &skos("related"), &ex("d")),
        t(&ex("a"), &skos("prefLabel"), "\"A\"@en"),
        t(&ex("s"), &skos("hasTopConcept"), &ex("a")),
    ]);
    for want in [
        t(&ex("b"), &skos("narrower"), &ex("a")),
        t(&ex("a"), &skos("broaderTransitive"), &ex("c")),
        t(&ex("c"), &skos("narrowerTransitive"), &ex("a")),
        t(&ex("d"), &skos("related"), &ex("a")),
        t(&ex("a"), &skos("semanticRelation"), &ex("d")),
        t(&ex("a"), RDFS_LABEL, "\"A\"@en"),
        t(&ex("a"), &skos("inScheme"), &ex("s")),
    ] {
        assert!(c.contains(&want), "missing {want:?}");
    }
}

/// SSSOM RG2 turns `subClassOf` into `broadMatch`; the trimmed schema must not
/// lift that into `skos:broader` or type the classes as `skos:Concept`.
#[test]
fn class_hierarchy_does_not_leak_into_concepts() {
    let c = close(vec![t(&ex("Dog"), SUB_CLASS_OF, &ex("Animal"))]);
    assert!(c.contains(&t(&ex("Dog"), &skos("broadMatch"), &ex("Animal"))));
    assert!(!c.contains(&t(&ex("Dog"), &skos("broader"), &ex("Animal"))));
    assert!(!c
        .iter()
        .any(|(_, p, o)| p == RDF_TYPE && *o == skos("Concept")));
}

#[test]
fn concept_and_collection_are_disjoint() {
    let mut e = Engine::new();
    e.load_base(skos_core().into_iter().chain([
        t(&ex("x"), RDF_TYPE, &skos("Concept")),
        t(&ex("x"), RDF_TYPE, &skos("OrderedCollection")),
    ]))
    .unwrap();
    assert!(!e.is_consistent().unwrap());
}
