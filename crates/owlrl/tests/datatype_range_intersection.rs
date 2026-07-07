//! End-to-end `Engine` tests for value-space intersection narrowing of
//! `rdfs:range` declarations (`crates/owlrl/src/datatype_ranges.rs`, #160).
//!
//! Covers the two W3C conformance cases this pass exists for
//! (`WebOnt-I5.8-008-pe`, `WebOnt-I5.8-009-pe`) plus guard cases proving the
//! pass stays silent outside its narrow trigger condition (a single
//! declared range, or a declared range that includes an opaque datatype).

use horndb_owlrl::integration::Engine;
use oxrdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad};

const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const XSD_SHORT: &str = "http://www.w3.org/2001/XMLSchema#short";
const XSD_BYTE: &str = "http://www.w3.org/2001/XMLSchema#byte";
const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#int";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_UNSIGNED_INT: &str = "http://www.w3.org/2001/XMLSchema#unsignedInt";
const XSD_UNSIGNED_SHORT: &str = "http://www.w3.org/2001/XMLSchema#unsignedShort";
const XSD_NON_NEGATIVE_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";
const XSD_NON_POSITIVE_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#nonPositiveInteger";

/// A `p rdfs:range D` (or any other named-node/named-node) triple.
fn nq(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        NamedNode::new(o).unwrap(),
        GraphName::DefaultGraph,
    )
}

fn dataset(quads: &[Quad]) -> Dataset {
    let mut d = Dataset::new();
    for q in quads {
        d.insert(q);
    }
    d
}

/// `WebOnt-I5.8-008-pe`: `p rdfs:range xsd:short` + `p rdfs:range
/// xsd:unsignedInt` entails `p rdfs:range xsd:unsignedShort` (short ∩
/// unsignedInt = `[0, 32767]` ⊆ unsignedShort's `[0, 65535]`).
#[test]
fn webont_i58_008_short_and_unsigned_int_entail_unsigned_short() {
    let mut engine = Engine::new();
    let premise = dataset(&[
        nq("http://ex/p", RDFS_RANGE, XSD_SHORT),
        nq("http://ex/p", RDFS_RANGE, XSD_UNSIGNED_INT),
    ]);
    engine.load(&premise).unwrap();

    let conclusion = dataset(&[nq("http://ex/p", RDFS_RANGE, XSD_UNSIGNED_SHORT)]);
    assert!(
        engine.entails(&conclusion).unwrap(),
        "short ∩ unsignedInt should entail rdfs:range xsd:unsignedShort"
    );
    assert!(engine.is_consistent().unwrap());
}

/// `WebOnt-I5.8-009-pe`: `p rdfs:range xsd:nonNegativeInteger` + `p
/// rdfs:range xsd:nonPositiveInteger` entails `p rdfs:range xsd:short`
/// (`[0, ∞) ∩ (−∞, 0] = {0}` ⊆ short's `[−32768, 32767]`).
#[test]
fn webont_i58_009_nonneg_and_nonpos_entail_short() {
    let mut engine = Engine::new();
    let premise = dataset(&[
        nq("http://ex/p", RDFS_RANGE, XSD_NON_NEGATIVE_INTEGER),
        nq("http://ex/p", RDFS_RANGE, XSD_NON_POSITIVE_INTEGER),
    ]);
    engine.load(&premise).unwrap();

    let conclusion = dataset(&[nq("http://ex/p", RDFS_RANGE, XSD_SHORT)]);
    assert!(
        engine.entails(&conclusion).unwrap(),
        "nonNegativeInteger ∩ nonPositiveInteger should entail rdfs:range xsd:short"
    );
    assert!(engine.is_consistent().unwrap());
}

/// Guard: a property with a **single** declared range must not get spurious
/// narrowing from this pass. `scm-rng1` still broadens `short` up to `int`
/// etc., but nothing narrows it down to `byte`.
#[test]
fn single_range_does_not_spuriously_narrow() {
    let mut engine = Engine::new();
    let premise = dataset(&[nq("http://ex/p", RDFS_RANGE, XSD_SHORT)]);
    engine.load(&premise).unwrap();

    let bogus = dataset(&[nq("http://ex/p", RDFS_RANGE, XSD_BYTE)]);
    assert!(
        !engine.entails(&bogus).unwrap(),
        "a single declared range xsd:short must not entail the narrower xsd:byte"
    );
    // scm-rng1 broadening should still work as before.
    let broadened = dataset(&[nq("http://ex/p", RDFS_RANGE, XSD_INT)]);
    assert!(
        engine.entails(&broadened).unwrap(),
        "scm-rng1 should still broaden xsd:short up to xsd:int"
    );
}

/// Guard: a property whose declared ranges include an opaque datatype
/// (`xsd:string`) derives nothing from this pass, even though the other
/// declared range (`xsd:int`) is numeric.
#[test]
fn opaque_range_derives_nothing() {
    let mut engine = Engine::new();
    let premise = dataset(&[
        nq("http://ex/p", RDFS_RANGE, XSD_STRING),
        nq("http://ex/p", RDFS_RANGE, XSD_INT),
    ]);
    engine.load(&premise).unwrap();

    for bogus in [XSD_BYTE, XSD_SHORT, XSD_UNSIGNED_SHORT] {
        let conclusion = dataset(&[nq("http://ex/p", RDFS_RANGE, bogus)]);
        assert!(
            !engine.entails(&conclusion).unwrap(),
            "xsd:string + xsd:int must not narrow to {bogus}"
        );
    }
    assert!(engine.is_consistent().unwrap());
}
