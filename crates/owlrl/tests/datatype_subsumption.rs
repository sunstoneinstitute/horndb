//! Integration tests for OWL 2 RL datatype reasoning over datatype IRIs
//! (dt-type1 + dt-type2 as a subsumption lattice), wired through the
//! `Engine` façade in `integration.rs`.
//!
//! These reason over datatype *IRIs* as ordinary terms — there is no
//! literal value parsing/comparison here. The lattice edges are injected
//! at load time and the existing `scm-rng1` / `scm-sco` rules propagate
//! them along `rdfs:range`.

use horndb_owlrl::integration::Engine;
use oxrdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const RDFS_DATATYPE: &str = "http://www.w3.org/2000/01/rdf-schema#Datatype";
const OWL_DATATYPE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#DatatypeProperty";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#int";
const XSD_SHORT: &str = "http://www.w3.org/2001/XMLSchema#short";
const XSD_BYTE: &str = "http://www.w3.org/2001/XMLSchema#byte";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
const XSD_DATE_TIME_STAMP: &str = "http://www.w3.org/2001/XMLSchema#dateTimeStamp";

fn nq(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        NamedNode::new(o).unwrap(),
        GraphName::DefaultGraph,
    )
}

/// dt-type1: each datatype `D` is declared `D rdf:type rdfs:Datatype`,
/// even with no premise data. Mirrors WebOnt-I5.8-011.
#[test]
fn dt_type1_declares_datatypes() {
    let mut engine = Engine::new();
    engine.load(&Dataset::new()).unwrap();

    let mut concl = Dataset::new();
    concl.insert(&nq(XSD_INTEGER, RDF_TYPE, RDFS_DATATYPE));
    assert!(
        engine.entails(&concl).unwrap(),
        "xsd:integer should be declared rdf:type rdfs:Datatype"
    );

    let mut concl = Dataset::new();
    concl.insert(&nq(XSD_STRING, RDF_TYPE, RDFS_DATATYPE));
    assert!(
        engine.entails(&concl).unwrap(),
        "xsd:string should be declared rdf:type rdfs:Datatype"
    );
}

/// dt-type2: range propagates up the XSD subsumption lattice via the
/// existing `scm-rng1` rule. Mirrors WebOnt-I5.8-006.
#[test]
fn dt_type2_range_subsumption() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&nq("http://ex/p", RDF_TYPE, OWL_DATATYPE_PROPERTY));
    premise.insert(&nq("http://ex/p", RDFS_RANGE, XSD_BYTE));
    engine.load(&premise).unwrap();

    let mut concl = Dataset::new();
    concl.insert(&nq("http://ex/p", RDFS_RANGE, XSD_SHORT));
    assert!(
        engine.entails(&concl).unwrap(),
        "range xsd:byte should propagate to xsd:short (byte ⊑ short)"
    );
}

/// dt-type2 transitively: byte ⊑ short ⊑ int ⊑ long ⊑ integer, so range
/// xsd:byte entails range xsd:int and range xsd:integer.
#[test]
fn dt_type2_range_transitive() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&nq("http://ex/p", RDF_TYPE, OWL_DATATYPE_PROPERTY));
    premise.insert(&nq("http://ex/p", RDFS_RANGE, XSD_BYTE));
    engine.load(&premise).unwrap();

    let mut concl = Dataset::new();
    concl.insert(&nq("http://ex/p", RDFS_RANGE, XSD_INT));
    assert!(
        engine.entails(&concl).unwrap(),
        "range xsd:byte should propagate transitively to xsd:int"
    );

    let mut concl = Dataset::new();
    concl.insert(&nq("http://ex/p", RDFS_RANGE, XSD_INTEGER));
    assert!(
        engine.entails(&concl).unwrap(),
        "range xsd:byte should propagate transitively to xsd:integer"
    );
}

/// dt-type2 over the non-numeric branch: `xsd:dateTimeStamp ⊑ xsd:dateTime`
/// (dateTimeStamp is dateTime with a required timezone). dt-type1 declares
/// it a datatype and range propagates to xsd:dateTime.
#[test]
fn dt_type2_date_time_stamp_subsumption() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&nq("http://ex/p", RDF_TYPE, OWL_DATATYPE_PROPERTY));
    premise.insert(&nq("http://ex/p", RDFS_RANGE, XSD_DATE_TIME_STAMP));
    engine.load(&premise).unwrap();

    // dt-type1: dateTimeStamp is declared a datatype.
    let mut concl = Dataset::new();
    concl.insert(&nq(XSD_DATE_TIME_STAMP, RDF_TYPE, RDFS_DATATYPE));
    assert!(
        engine.entails(&concl).unwrap(),
        "xsd:dateTimeStamp should be declared rdf:type rdfs:Datatype"
    );

    // dt-type2: range propagates to the wider xsd:dateTime.
    let mut concl = Dataset::new();
    concl.insert(&nq("http://ex/p", RDFS_RANGE, XSD_DATE_TIME));
    assert!(
        engine.entails(&concl).unwrap(),
        "range xsd:dateTimeStamp should propagate to xsd:dateTime (dateTimeStamp ⊑ dateTime)"
    );
}

/// dt-type2 must not cross branches: xsd:byte is not subsumed by
/// xsd:string, so range xsd:byte does NOT entail range xsd:string.
#[test]
fn dt_type2_no_cross_branch() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&nq("http://ex/p", RDF_TYPE, OWL_DATATYPE_PROPERTY));
    premise.insert(&nq("http://ex/p", RDFS_RANGE, XSD_BYTE));
    engine.load(&premise).unwrap();

    let mut concl = Dataset::new();
    concl.insert(&nq("http://ex/p", RDFS_RANGE, XSD_STRING));
    assert!(
        !engine.entails(&concl).unwrap(),
        "range xsd:byte must NOT propagate to the unrelated xsd:string branch"
    );
}
