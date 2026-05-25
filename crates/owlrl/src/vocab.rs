//! OWL / RDF / RDFS vocabulary IDs the generated rules need to consult.
//!
//! At runtime, populated by the caller (typically SPEC-02 storage layer) by
//! dictionary-encoding each IRI. In tests we populate it by hand.
//!
//! Each field carries a `///` doc comment containing the canonical QName in
//! backticks (e.g. `` `rdf:type` ``). `build.rs` parses this file via syn
//! and uses those QNames to auto-generate the rules-parser lookup table —
//! so adding a vocabulary term is a single edit in *this* file: add a
//! field with its QName doc, then a matching line in `synthetic()` below.
//!
//! See `crates/owlrl/AGENTS.md` for the full pipeline description.

use crate::types::TermId;

/// All vocabulary terms referenced by the Stage-1 OWL 2 RL rule subset.
/// Fields are public so a builder can fill them directly.
#[derive(Copy, Clone, Debug)]
pub struct Vocabulary {
    /// `rdf:type`
    pub rdf_type: TermId,
    /// `rdf:first`
    pub rdf_first: TermId,
    /// `rdf:rest`
    pub rdf_rest: TermId,
    /// `rdf:nil`
    pub rdf_nil: TermId,

    /// `rdfs:subClassOf`
    pub rdfs_sub_class_of: TermId,
    /// `rdfs:subPropertyOf`
    pub rdfs_sub_property_of: TermId,
    /// `rdfs:domain`
    pub rdfs_domain: TermId,
    /// `rdfs:range`
    pub rdfs_range: TermId,

    /// `owl:Class`
    pub owl_class: TermId,
    /// `owl:Thing`
    pub owl_thing: TermId,
    /// `owl:Nothing`
    pub owl_nothing: TermId,

    /// `owl:sameAs`
    pub owl_same_as: TermId,
    /// `owl:differentFrom`
    pub owl_different_from: TermId,
    /// `owl:equivalentClass`
    pub owl_equivalent_class: TermId,
    /// `owl:equivalentProperty`
    pub owl_equivalent_property: TermId,
    /// `owl:inverseOf`
    pub owl_inverse_of: TermId,

    /// `owl:FunctionalProperty`
    pub owl_functional_property: TermId,
    /// `owl:InverseFunctionalProperty`
    pub owl_inverse_functional_property: TermId,
    /// `owl:SymmetricProperty`
    pub owl_symmetric_property: TermId,
    /// `owl:TransitiveProperty`
    pub owl_transitive_property: TermId,
    /// `owl:IrreflexiveProperty`
    pub owl_irreflexive_property: TermId,
    /// `owl:ReflexiveProperty`
    pub owl_reflexive_property: TermId,
    /// `owl:AsymmetricProperty`
    pub owl_asymmetric_property: TermId,

    /// `owl:propertyDisjointWith`
    pub owl_property_disjoint_with: TermId,
    /// `owl:disjointWith`
    pub owl_disjoint_with: TermId,
    /// `owl:complementOf`
    pub owl_complement_of: TermId,

    /// `owl:intersectionOf`
    pub owl_intersection_of: TermId,
    /// `owl:unionOf`
    pub owl_union_of: TermId,

    /// `owl:someValuesFrom`
    pub owl_some_values_from: TermId,
    /// `owl:allValuesFrom`
    pub owl_all_values_from: TermId,
    /// `owl:hasValue`
    pub owl_has_value: TermId,
    /// `owl:onProperty`
    pub owl_on_property: TermId,
    /// `owl:maxCardinality`
    pub owl_max_cardinality: TermId,

    /// `owl:sourceIndividual`
    pub owl_source_individual: TermId,
    /// `owl:assertionProperty`
    pub owl_assertion_property: TermId,
    /// `owl:targetIndividual`
    pub owl_target_individual: TermId,
    /// `owl:targetValue`
    pub owl_target_value: TermId,

    /// `owl:ObjectProperty`
    pub owl_object_property: TermId,

    // list-axiom rules (SPEC-04 F1, list_rules.rs)
    /// `owl:propertyChainAxiom`
    pub owl_property_chain_axiom: TermId,
    /// `owl:hasKey`
    pub owl_has_key: TermId,
    /// `owl:AllDisjointClasses`
    pub owl_all_disjoint_classes: TermId,
    /// `owl:AllDifferent`
    pub owl_all_different: TermId,
    /// `owl:members`
    pub owl_members: TermId,
    /// `owl:distinctMembers`
    pub owl_distinct_members: TermId,
    /// `owl:NamedIndividual`
    pub owl_named_individual: TermId,
}

impl Vocabulary {
    /// Construct a vocabulary by allocating consecutive `TermId`s starting from
    /// `base`. Used by tests; production code receives the real IDs from the
    /// SPEC-02 dictionary.
    pub fn synthetic(base: u64) -> Self {
        let mut n = base;
        let mut next = || {
            let v = TermId(n);
            n += 1;
            v
        };
        Self {
            rdf_type: next(),
            rdf_first: next(),
            rdf_rest: next(),
            rdf_nil: next(),
            rdfs_sub_class_of: next(),
            rdfs_sub_property_of: next(),
            rdfs_domain: next(),
            rdfs_range: next(),
            owl_class: next(),
            owl_thing: next(),
            owl_nothing: next(),
            owl_same_as: next(),
            owl_different_from: next(),
            owl_equivalent_class: next(),
            owl_equivalent_property: next(),
            owl_inverse_of: next(),
            owl_functional_property: next(),
            owl_inverse_functional_property: next(),
            owl_symmetric_property: next(),
            owl_transitive_property: next(),
            owl_irreflexive_property: next(),
            owl_reflexive_property: next(),
            owl_asymmetric_property: next(),
            owl_property_disjoint_with: next(),
            owl_disjoint_with: next(),
            owl_complement_of: next(),
            owl_intersection_of: next(),
            owl_union_of: next(),
            owl_some_values_from: next(),
            owl_all_values_from: next(),
            owl_has_value: next(),
            owl_on_property: next(),
            owl_max_cardinality: next(),
            owl_source_individual: next(),
            owl_assertion_property: next(),
            owl_target_individual: next(),
            owl_target_value: next(),
            owl_object_property: next(),
            owl_property_chain_axiom: next(),
            owl_has_key: next(),
            owl_all_disjoint_classes: next(),
            owl_all_different: next(),
            owl_members: next(),
            owl_distinct_members: next(),
            owl_named_individual: next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_yields_distinct_ids() {
        let v = Vocabulary::synthetic(100);
        assert_eq!(v.rdf_type, TermId(100));
        assert_ne!(v.rdf_type, v.rdfs_sub_class_of);
        assert_ne!(v.owl_thing, v.owl_nothing);
    }
}
