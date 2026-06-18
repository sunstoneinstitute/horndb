//! Integration tests for the OWL 2 RL literal-value datatype rules
//! (`dt-eq`, `dt-diff`, `dt-not-type`), wired through the `Engine` façade.
//!
//! Unlike `datatype_subsumption.rs` (which reasons over datatype IRIs), these
//! reason over the *values* literals denote: cross-lexical / cross-datatype
//! equality (`dt-eq`), value disequality (`dt-diff`), and ill-typed literals
//! (`dt-not-type`). The conclusions are injected at load time and propagated
//! by the compiled `eq-diff1` / `eq-rep-*` rules.

use horndb_owlrl::integration::Engine;
use oxrdf::{Dataset, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_DIFFERENT_FROM: &str = "http://www.w3.org/2002/07/owl#differentFrom";
const OWL_FUNCTIONAL_PROPERTY: &str = "http://www.w3.org/2002/07/owl#FunctionalProperty";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_BYTE: &str = "http://www.w3.org/2001/XMLSchema#byte";

/// A triple whose object is a named node.
fn nq(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        NamedNode::new(o).unwrap(),
        GraphName::DefaultGraph,
    )
}

/// A triple whose object is a typed literal.
fn lit(s: &str, p: &str, value: &str, datatype: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        Literal::new_typed_literal(value, NamedNode::new(datatype).unwrap()),
        GraphName::DefaultGraph,
    )
}

/// The N-Triples-style dictionary key for a typed literal (matches
/// `Engine::materialized_triples` output for literal terms).
fn typed_key(value: &str, datatype: &str) -> String {
    format!("\"{value}\"^^<{datatype}>")
}

/// True iff the materialised closure contains a triple with the given lexical
/// subject / predicate / object. Used to assert over literal-subject triples,
/// which oxrdf's `Quad` cannot represent (so `entails` cannot check them).
fn has_triple(engine: &Engine, s: &str, p: &str, o: &str) -> bool {
    engine
        .materialized_triples()
        .unwrap()
        .iter()
        .any(|(ts, tp, to)| ts == s && tp == p && to == o)
}

/// dt-eq: two integer literals with the same value but different lexical forms
/// (`"1"` and `"01"`) are inferred `owl:sameAs`.
#[test]
fn dt_eq_cross_lexical_integers() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "1", XSD_INTEGER));
    premise.insert(&lit("http://ex/b", "http://ex/p", "01", XSD_INTEGER));
    engine.load(&premise).unwrap();

    // The two literal terms should be owl:sameAs each other.
    assert!(
        has_triple(
            &engine,
            &typed_key("1", XSD_INTEGER),
            OWL_SAME_AS,
            &typed_key("01", XSD_INTEGER)
        ),
        "\"1\" and \"01\" (xsd:integer) should be owl:sameAs (dt-eq)"
    );
    assert!(
        engine.is_consistent().unwrap(),
        "value-equal literals are not an inconsistency"
    );
}

/// dt-eq across the integer tower: `"1"^^xsd:byte` ≡ `"1"^^xsd:integer`.
#[test]
fn dt_eq_cross_datatype_integers() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "1", XSD_BYTE));
    premise.insert(&lit("http://ex/b", "http://ex/p", "1", XSD_INTEGER));
    engine.load(&premise).unwrap();

    assert!(
        has_triple(
            &engine,
            &typed_key("1", XSD_BYTE),
            OWL_SAME_AS,
            &typed_key("1", XSD_INTEGER)
        ),
        "\"1\"^^xsd:byte and \"1\"^^xsd:integer should be owl:sameAs (dt-eq)"
    );
}

/// dt-diff: two integer literals with different values are `owl:differentFrom`.
#[test]
fn dt_diff_distinct_integers() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "1", XSD_INTEGER));
    premise.insert(&lit("http://ex/b", "http://ex/p", "2", XSD_INTEGER));
    engine.load(&premise).unwrap();

    assert!(
        has_triple(
            &engine,
            &typed_key("1", XSD_INTEGER),
            OWL_DIFFERENT_FROM,
            &typed_key("2", XSD_INTEGER)
        ),
        "\"1\" and \"2\" (xsd:integer) should be owl:differentFrom (dt-diff)"
    );
}

/// dt-not-type: an integer-typed literal whose lexical form is not in the
/// integer value space (`"abc"^^xsd:integer`) makes the graph inconsistent.
#[test]
fn dt_not_type_ill_typed_integer_is_inconsistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "abc", XSD_INTEGER));
    engine.load(&premise).unwrap();
    assert!(
        !engine.is_consistent().unwrap(),
        "\"abc\"^^xsd:integer is ill-typed (dt-not-type) → inconsistent"
    );
}

/// dt-not-type for an out-of-range bounded subtype: `"999"^^xsd:byte` is
/// outside `xsd:byte`'s [-128, 127] value space → inconsistent.
#[test]
fn dt_not_type_out_of_range_byte_is_inconsistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "999", XSD_BYTE));
    engine.load(&premise).unwrap();
    assert!(
        !engine.is_consistent().unwrap(),
        "\"999\"^^xsd:byte is out of range (dt-not-type) → inconsistent"
    );
}

/// dt-not-type over a *derived* datatype membership: `:p rdfs:range xsd:byte`
/// types the value `"999"^^xsd:integer` as `xsd:byte` via `prp-rng`, and 999 is
/// outside `xsd:byte`'s [-128, 127] value space → inconsistent.
#[test]
fn dt_not_type_via_derived_range_membership_is_inconsistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    // p has range xsd:byte.
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/p").unwrap()),
        NamedNode::new("http://www.w3.org/2000/01/rdf-schema#range").unwrap(),
        NamedNode::new(XSD_BYTE).unwrap(),
        GraphName::DefaultGraph,
    ));
    // s p "999"^^xsd:integer — the literal is well-typed as xsd:integer, but
    // prp-rng types it xsd:byte, where 999 is out of range.
    premise.insert(&lit("http://ex/s", "http://ex/p", "999", XSD_INTEGER));
    engine.load(&premise).unwrap();
    assert!(
        !engine.is_consistent().unwrap(),
        "\"999\"^^xsd:integer typed xsd:byte via prp-rng is out of range (dt-not-type) → inconsistent"
    );
}

/// dt-not-type over a derived membership that crosses value spaces: a *string*
/// literal `"5"^^xsd:string` typed `xsd:integer` via `prp-rng` denotes a string
/// value, which is not in the integer value space → inconsistent (even though
/// the lexical form "5" would re-parse as an integer).
#[test]
fn dt_not_type_string_typed_as_integer_is_inconsistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/p").unwrap()),
        NamedNode::new("http://www.w3.org/2000/01/rdf-schema#range").unwrap(),
        NamedNode::new(XSD_INTEGER).unwrap(),
        GraphName::DefaultGraph,
    ));
    let xsd_string = "http://www.w3.org/2001/XMLSchema#string";
    premise.insert(&lit("http://ex/s", "http://ex/p", "5", xsd_string));
    engine.load(&premise).unwrap();
    assert!(
        !engine.is_consistent().unwrap(),
        "\"5\"^^xsd:string typed xsd:integer via prp-rng denotes a string value (dt-not-type) → inconsistent"
    );
}

/// Companion consistency guard: a *well-typed* derived membership stays
/// consistent. `:p rdfs:range xsd:byte` with `"5"^^xsd:integer` types "5" as
/// xsd:byte, and 5 is in range.
#[test]
fn well_typed_derived_range_membership_stays_consistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/p").unwrap()),
        NamedNode::new("http://www.w3.org/2000/01/rdf-schema#range").unwrap(),
        NamedNode::new(XSD_BYTE).unwrap(),
        GraphName::DefaultGraph,
    ));
    premise.insert(&lit("http://ex/s", "http://ex/p", "5", XSD_INTEGER));
    engine.load(&premise).unwrap();
    assert!(
        engine.is_consistent().unwrap(),
        "\"5\"^^xsd:integer typed xsd:byte via prp-rng is in range → consistent"
    );
}

/// A large unbounded integer must not be flagged as ill-typed (regression for
/// the i128-overflow false inconsistency).
#[test]
fn large_unbounded_integer_stays_consistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    let big = "123456789012345678901234567890123456789012345678901234567890";
    premise.insert(&lit("http://ex/s", "http://ex/p", big, XSD_INTEGER));
    engine.load(&premise).unwrap();
    assert!(
        engine.is_consistent().unwrap(),
        "a 60-digit xsd:integer is a valid value, not an inconsistency"
    );
}

/// The New-Feature-Keys-006 scenario: a functional property `hasName` with two
/// distinct string values for the same subject collapses (prp-fp) to
/// `"Peter" owl:sameAs "Kichwa-Tembo"`, while dt-diff derives they are
/// `owl:differentFrom`; eq-diff1 then makes the graph inconsistent.
#[test]
fn keys_006_functional_property_literal_collision_is_inconsistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    // hasName is a functional property.
    premise.insert(&nq(
        "http://example.org/hasName",
        RDF_TYPE,
        OWL_FUNCTIONAL_PROPERTY,
    ));
    // Peter hasName "Peter" and "Kichwa-Tembo" (two distinct string literals).
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://example.org/Peter").unwrap()),
        NamedNode::new("http://example.org/hasName").unwrap(),
        Literal::new_simple_literal("Peter"),
        GraphName::DefaultGraph,
    ));
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://example.org/Peter").unwrap()),
        NamedNode::new("http://example.org/hasName").unwrap(),
        Literal::new_simple_literal("Kichwa-Tembo"),
        GraphName::DefaultGraph,
    ));
    engine.load(&premise).unwrap();
    assert!(
        !engine.is_consistent().unwrap(),
        "functional property with two distinct literal values must be inconsistent (prp-fp + dt-diff + eq-diff1)"
    );
}

/// Distinct strings under a *plain* literal (no datatype) are still compared as
/// strings and declared differentFrom — `Literal::new_simple_literal` yields an
/// `xsd:string`.
#[test]
fn dt_diff_distinct_plain_strings() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/a").unwrap()),
        NamedNode::new("http://ex/p").unwrap(),
        Literal::new_simple_literal("foo"),
        GraphName::DefaultGraph,
    ));
    premise.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/b").unwrap()),
        NamedNode::new("http://ex/p").unwrap(),
        Literal::new_simple_literal("bar"),
        GraphName::DefaultGraph,
    ));
    engine.load(&premise).unwrap();

    let xsd_string = "http://www.w3.org/2001/XMLSchema#string";
    assert!(
        has_triple(
            &engine,
            &typed_key("foo", xsd_string),
            OWL_DIFFERENT_FROM,
            &typed_key("bar", xsd_string)
        ),
        "distinct plain strings should be owl:differentFrom (dt-diff)"
    );
    assert!(
        engine.is_consistent().unwrap(),
        "two distinct strings that are never sameAs are consistent"
    );
}

/// A consistency guard: a well-typed single literal, and value-equal literals,
/// do not spuriously trip inconsistency.
#[test]
fn well_typed_literals_stay_consistent() {
    let mut engine = Engine::new();
    let mut premise = Dataset::new();
    premise.insert(&lit("http://ex/a", "http://ex/p", "42", XSD_INTEGER));
    premise.insert(&lit("http://ex/b", "http://ex/p", "42", XSD_INTEGER));
    engine.load(&premise).unwrap();
    assert!(
        engine.is_consistent().unwrap(),
        "well-typed, value-equal literals must remain consistent"
    );
}
