//! OWL / RDF / RDFS vocabulary IDs the generated rules need to consult.
//!
//! At runtime, populated by the caller (typically SPEC-02 storage layer) by
//! dictionary-encoding each IRI. In tests we populate it by hand.

use crate::types::TermId;

/// All vocabulary terms referenced by the Stage-1 OWL 2 RL rule subset.
/// Fields are public so a builder can fill them directly.
#[derive(Copy, Clone, Debug)]
pub struct Vocabulary {
    // rdf:
    pub rdf_type: TermId,
    pub rdf_first: TermId,
    pub rdf_rest: TermId,
    pub rdf_nil: TermId,

    // rdfs:
    pub rdfs_sub_class_of: TermId,
    pub rdfs_sub_property_of: TermId,
    pub rdfs_domain: TermId,
    pub rdfs_range: TermId,

    // owl:
    pub owl_class: TermId,
    pub owl_thing: TermId,
    pub owl_nothing: TermId,
    pub owl_same_as: TermId,
    pub owl_different_from: TermId,
    pub owl_equivalent_class: TermId,
    pub owl_equivalent_property: TermId,
    pub owl_inverse_of: TermId,
    pub owl_functional_property: TermId,
    pub owl_inverse_functional_property: TermId,
    pub owl_symmetric_property: TermId,
    pub owl_transitive_property: TermId,
    pub owl_irreflexive_property: TermId,
    pub owl_asymmetric_property: TermId,
    pub owl_property_disjoint_with: TermId,
    pub owl_disjoint_with: TermId,
    pub owl_complement_of: TermId,
    pub owl_intersection_of: TermId,
    pub owl_union_of: TermId,
    pub owl_some_values_from: TermId,
    pub owl_all_values_from: TermId,
    pub owl_has_value: TermId,
    pub owl_on_property: TermId,
    pub owl_max_cardinality: TermId,
    pub owl_object_property: TermId,
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
            owl_object_property: next(),
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
