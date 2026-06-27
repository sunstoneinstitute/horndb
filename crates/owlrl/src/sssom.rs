//! SPEC-11 F2/F7/F8 — SSSOM representation glue over the rule engine's
//! provenance: justification tagging, n-ary mapping-node construction, and
//! confidence combination. The chaining itself lives in `rules.toml`; this
//! module turns a derived mapping + its provenance into SSSOM-shaped facts.

use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;

/// The `semapv:*` justification a derived mapping carries, per SPEC-11 F3.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Justification {
    /// Derived by transitivity / role-chain / generalisation (T1, RCE, RG).
    Chaining,
    /// Derived by inversion (RI1-5).
    Inversion,
}

impl Justification {
    /// The vocab `TermId` for this justification (a `semapv:` individual).
    pub fn term(self, v: &Vocabulary) -> TermId {
        match self {
            Justification::Chaining => v.semapv_mapping_chaining,
            Justification::Inversion => v.semapv_mapping_inversion,
        }
    }
}

/// Map a chaining rule id to its SSSOM justification. Returns `None` for
/// non-mapping (ordinary OWL-RL) rule ids.
pub fn rule_justification(rule_id: &str) -> Option<Justification> {
    match rule_id {
        id if id.starts_with("sssom-ri") => Some(Justification::Inversion),
        id if id.starts_with("sssom-rg")
            || id.starts_with("sssom-rce")
            || id.starts_with("sssom-t1")
            || id.starts_with("sssom-neg") =>
        {
            Some(Justification::Chaining)
        }
        _ => None,
    }
}

/// Combine confidences along a chain. SPEC-11 F7: product (independent-
/// probability) by default; unspecified confidence defaults to 1.0.
pub fn combine_confidence(premise_confidences: &[f64]) -> f64 {
    premise_confidences.iter().copied().product::<f64>()
}

/// The triples of an n-ary `sssom:Mapping` node for an inferred mapping
/// (SPEC-11 F2). `node` is a fresh blank/IRI TermId minted by the caller;
/// `derived_from` are the mapping-node ids of the premises (F8).
pub fn mapping_node_triples(
    v: &Vocabulary,
    node: TermId,
    subject: TermId,
    predicate: TermId,
    object: TermId,
    justification: Justification,
    derived_from: &[TermId],
) -> Vec<Triple> {
    let mut out = vec![
        Triple::new(node, v.rdf_type, v.sssom_mapping),
        Triple::new(node, v.sssom_subject_id, subject),
        Triple::new(node, v.sssom_predicate_id, predicate),
        Triple::new(node, v.sssom_object_id, object),
        Triple::new(node, v.sssom_mapping_justification, justification.term(v)),
    ];
    for &df in derived_from {
        out.push(Triple::new(node, v.sssom_derived_from, df));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn justification_mapping_is_correct() {
        assert_eq!(
            rule_justification("sssom-ri3"),
            Some(Justification::Inversion)
        );
        assert_eq!(
            rule_justification("sssom-rg1"),
            Some(Justification::Chaining)
        );
        assert_eq!(
            rule_justification("sssom-rce1-broad"),
            Some(Justification::Chaining)
        );
        assert_eq!(
            rule_justification("sssom-t1-exact"),
            Some(Justification::Chaining)
        );
        assert_eq!(
            rule_justification("sssom-neg-exact"),
            Some(Justification::Chaining)
        );
        assert_eq!(rule_justification("cax-sco"), None);
    }

    #[test]
    fn confidence_combines_by_product_with_unit_default() {
        assert_eq!(combine_confidence(&[]), 1.0);
        assert_eq!(combine_confidence(&[0.9]), 0.9);
        assert!((combine_confidence(&[0.9, 0.8]) - 0.72).abs() < 1e-12);
    }

    #[test]
    fn mapping_node_emits_canonical_shape() {
        let v = Vocabulary::synthetic(10_000);
        let node = TermId(1);
        let triples = mapping_node_triples(
            &v,
            node,
            TermId(2),
            v.skos_broad_match,
            TermId(3),
            Justification::Chaining,
            &[TermId(7), TermId(8)],
        );
        assert!(triples.contains(&Triple::new(node, v.rdf_type, v.sssom_mapping)));
        assert!(triples.contains(&Triple::new(node, v.sssom_subject_id, TermId(2))));
        assert!(triples.contains(&Triple::new(
            node,
            v.sssom_mapping_justification,
            v.semapv_mapping_chaining
        )));
        assert!(triples.contains(&Triple::new(node, v.sssom_derived_from, TermId(7))));
        assert!(triples.contains(&Triple::new(node, v.sssom_derived_from, TermId(8))));
        assert_eq!(triples.len(), 7); // type + 3 slots + justification + 2 derived_from
    }
}
