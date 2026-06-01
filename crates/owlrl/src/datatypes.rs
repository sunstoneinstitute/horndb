//! OWL 2 RL datatype reasoning over datatype **IRIs** (not literal values).
//!
//! Implements two OWL 2 RL datatype rules as load-time base-axiom
//! injection:
//!
//! - **dt-type1**: each datatype `D` in the OWL 2 RL datatype set is
//!   declared `D rdf:type rdfs:Datatype`.
//! - **dt-type2** (as a subsumption lattice): the XSD datatype hierarchy is
//!   injected as `rdfs:subClassOf` edges. Only the numeric tower carries
//!   subsumption edges; non-numeric datatypes (`xsd:string`, `xsd:boolean`,
//!   `xsd:dateTime`) are declared by dt-type1 but have no edges. The
//!   existing `scm-rng1` rule (range narrows along `subClassOf` of the
//!   range class) and `scm-sco` (transitive `subClassOf` closure) then do
//!   the propagation — this module does not reimplement them.
//!
//! This is **not** literal value reasoning: the datatypes are treated as
//! ordinary terms and the lattice is the fixed XSD datatype hierarchy from
//! the OWL 2 datatype map. Literal value parsing/comparison is a separate
//! concern (Stage-2, tracked elsewhere).

use crate::store::MemStore;
use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;

/// `rdfs:Datatype` — the class every declared datatype is a member of.
pub const RDFS_DATATYPE: &str = "http://www.w3.org/2000/01/rdf-schema#Datatype";

#[cfg(test)]
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

macro_rules! xsd {
    ($name:literal) => {
        concat!("http://www.w3.org/2001/XMLSchema#", $name)
    };
}

/// The OWL 2 RL datatypes declared via dt-type1 (`D rdf:type
/// rdfs:Datatype`). This is the subset of the OWL 2 datatype map this
/// stage reasons over as IRIs; every IRI appearing in
/// [`XSD_SUBCLASS_EDGES`] is also present here.
pub const XSD_DATATYPES: &[&str] = &[
    xsd!("string"),
    xsd!("boolean"),
    xsd!("decimal"),
    xsd!("integer"),
    xsd!("dateTime"),
    xsd!("long"),
    xsd!("int"),
    xsd!("short"),
    xsd!("byte"),
    xsd!("nonNegativeInteger"),
    xsd!("positiveInteger"),
    xsd!("unsignedLong"),
    xsd!("unsignedInt"),
    xsd!("unsignedShort"),
    xsd!("unsignedByte"),
    xsd!("nonPositiveInteger"),
    xsd!("negativeInteger"),
];

/// Directed `(sub, super)` `rdfs:subClassOf` edges of the XSD numeric
/// datatype lattice (dt-type2). Each pair `(a, b)` means `a ⊑ b`. The
/// transitive closure is left to `scm-sco`; only the immediate edges are
/// listed. No cross-branch or intersection edges.
pub const XSD_SUBCLASS_EDGES: &[(&str, &str)] = &[
    // integer ⊑ decimal
    (xsd!("integer"), xsd!("decimal")),
    // signed integer chain: byte ⊑ short ⊑ int ⊑ long ⊑ integer
    (xsd!("long"), xsd!("integer")),
    (xsd!("int"), xsd!("long")),
    (xsd!("short"), xsd!("int")),
    (xsd!("byte"), xsd!("short")),
    // nonNegativeInteger ⊑ integer
    (xsd!("nonNegativeInteger"), xsd!("integer")),
    // positiveInteger ⊑ nonNegativeInteger
    (xsd!("positiveInteger"), xsd!("nonNegativeInteger")),
    // unsigned chain: unsignedByte ⊑ unsignedShort ⊑ unsignedInt ⊑
    // unsignedLong ⊑ nonNegativeInteger
    (xsd!("unsignedLong"), xsd!("nonNegativeInteger")),
    (xsd!("unsignedInt"), xsd!("unsignedLong")),
    (xsd!("unsignedShort"), xsd!("unsignedInt")),
    (xsd!("unsignedByte"), xsd!("unsignedShort")),
    // nonPositiveInteger ⊑ integer; negativeInteger ⊑ nonPositiveInteger
    (xsd!("nonPositiveInteger"), xsd!("integer")),
    (xsd!("negativeInteger"), xsd!("nonPositiveInteger")),
];

/// Inject dt-type1 (`D rdf:type rdfs:Datatype`) and dt-type2 (the XSD
/// subsumption lattice as `rdfs:subClassOf` edges) as base axioms.
///
/// `intern` resolves an IRI to its `TermId` via the caller's dictionary
/// (allocating a fresh ID for IRIs not seen yet). The injected triples
/// are asserted as **base** facts — the same path the `owl:Thing`
/// inference helper uses — so they are schema-grade and drive
/// `scm-rng1` / `scm-sco` during materialization.
///
/// Injection is unconditional: it must run even for an empty premise so
/// dt-type1's datatype declarations are always present (per WebOnt-I5.8-011).
pub fn inject_datatype_axioms(
    store: &mut MemStore,
    vocab: &Vocabulary,
    mut intern: impl FnMut(&str) -> TermId,
) {
    let rdfs_datatype = intern(RDFS_DATATYPE);
    // dt-type1: `D rdf:type rdfs:Datatype` for each datatype IRI.
    for &dt in XSD_DATATYPES {
        let d = intern(dt);
        store.assert(Triple::new(d, vocab.rdf_type, rdfs_datatype));
    }
    // dt-type2: the lattice as `sub rdfs:subClassOf super` edges.
    for &(sub, sup) in XSD_SUBCLASS_EDGES {
        let s = intern(sub);
        let o = intern(sup);
        store.assert(Triple::new(s, vocab.rdfs_sub_class_of, o));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TripleStore;
    use rustc_hash::FxHashMap;

    /// Every IRI named in an edge must also be declared as a datatype.
    #[test]
    fn every_edge_endpoint_is_a_declared_datatype() {
        for &(sub, sup) in XSD_SUBCLASS_EDGES {
            assert!(
                XSD_DATATYPES.contains(&sub),
                "edge sub {sub} missing from XSD_DATATYPES"
            );
            assert!(
                XSD_DATATYPES.contains(&sup),
                "edge super {sup} missing from XSD_DATATYPES"
            );
        }
    }

    #[test]
    fn all_datatypes_share_the_xsd_namespace() {
        for &dt in XSD_DATATYPES {
            assert!(dt.starts_with(XSD), "{dt} is not in the xsd# namespace");
        }
    }

    #[test]
    fn inject_asserts_expected_base_triples() {
        let vocab = Vocabulary::synthetic(1);
        let mut store = MemStore::new(vocab);

        // A closure handing out incrementing synthetic TermIds, deduping
        // by IRI exactly like a real dictionary would.
        let mut dict: FxHashMap<String, TermId> = FxHashMap::default();
        let mut next: u64 = 1000;
        let intern = |iri: &str| -> TermId {
            if let Some(&t) = dict.get(iri) {
                return t;
            }
            let t = TermId(next);
            next += 1;
            dict.insert(iri.to_string(), t);
            t
        };

        inject_datatype_axioms(&mut store, &vocab, intern);

        // One dt-type1 triple per datatype + one subClassOf edge each.
        let all = store.all_triples();
        let expected = XSD_DATATYPES.len() + XSD_SUBCLASS_EDGES.len();
        assert_eq!(
            all.len(),
            expected,
            "expected {expected} injected base triples, got {}",
            all.len()
        );

        // Sample dt-type1: xsd:integer rdf:type rdfs:Datatype.
        let integer = dict[&format!("{XSD}integer")];
        let rdfs_dt = dict[RDFS_DATATYPE];
        assert!(store.contains(&Triple::new(integer, vocab.rdf_type, rdfs_dt)));

        // Sample dt-type2 edge: xsd:byte rdfs:subClassOf xsd:short.
        let byte = dict[&format!("{XSD}byte")];
        let short = dict[&format!("{XSD}short")];
        assert!(store.contains(&Triple::new(byte, vocab.rdfs_sub_class_of, short)));
    }
}
