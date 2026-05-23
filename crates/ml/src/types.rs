//! Shared value types for the ML integration boundary.
//!
//! Kept dependency-free so consuming crates can construct these
//! without dragging in anything ML-specific.

/// A confidence score in the closed interval [0.0, 1.0].
///
/// Constructed via [`Confidence::new`], which clamps out-of-range
/// and NaN inputs. We deliberately do *not* implement `Eq`; use
/// `PartialOrd` / `partial_cmp` for comparisons.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f64);

impl Confidence {
    pub fn new(v: f64) -> Self {
        if v.is_nan() {
            Confidence(0.0)
        } else if v < 0.0 {
            Confidence(0.0)
        } else if v > 1.0 {
            Confidence(1.0)
        } else {
            Confidence(v)
        }
    }

    pub fn zero() -> Self {
        Confidence(0.0)
    }

    pub fn one() -> Self {
        Confidence(1.0)
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Identity of an RDF subject as seen at the ML boundary.
///
/// We intentionally model this with owned `String`s rather than
/// dictionary IDs so this crate stays independent of SPEC-02 storage.
/// Consuming crates resolve to/from their dictionary at the call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TripleSubject {
    Iri(String),
    BlankNode(String),
}

/// Stable identity of an ML model contributing to the store.
///
/// Used both for provenance tagging (F5) and audit-log records (F6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(s: impl Into<String>) -> Self {
        ModelId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Coarse-grained shape of a subplan offered to the [`PlanAdvisor`].
///
/// We deliberately keep this opaque: the advisor sees structural
/// numbers but not the actual triple patterns. This lets us evolve
/// the planner's internal representation (SPEC-03 / SPEC-07) without
/// breaking ML plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubplanShape {
    pub n_patterns: usize,
    pub n_vars: usize,
    pub bound_vars: usize,
}

/// Advice returned by a [`PlanAdvisor`].
///
/// Every field is optional: the planner treats it as a hint and
/// always falls back to its own histograms when missing or implausible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanAdvice {
    pub estimated_cardinality: Option<u64>,
    pub suggested_index: Option<String>,
    /// Variable indices in the suggested binding order; empty = no opinion.
    pub suggested_join_order: Vec<usize>,
}

impl PlanAdvice {
    pub fn unadvised() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_to_unit_interval() {
        assert_eq!(Confidence::new(0.5).value(), 0.5);
        assert_eq!(Confidence::new(-0.1).value(), 0.0);
        assert_eq!(Confidence::new(1.7).value(), 1.0);
        assert_eq!(Confidence::new(f64::NAN).value(), 0.0);
    }

    #[test]
    fn confidence_zero_and_one_helpers() {
        assert_eq!(Confidence::zero().value(), 0.0);
        assert_eq!(Confidence::one().value(), 1.0);
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::new(0.1) < Confidence::new(0.9));
    }

    #[test]
    fn triple_subject_variants() {
        let iri = TripleSubject::Iri("http://example.org/a".into());
        let bnode = TripleSubject::BlankNode("b1".into());
        assert_ne!(iri, bnode);
        // Round-trip via clone.
        assert_eq!(iri.clone(), iri);
    }

    #[test]
    fn model_id_string_roundtrip() {
        let m = ModelId::new("faiss-mini-lm-v6");
        assert_eq!(m.as_str(), "faiss-mini-lm-v6");
    }

    #[test]
    fn subplan_shape_constructs() {
        let shape = SubplanShape {
            n_patterns: 4,
            n_vars: 3,
            bound_vars: 1,
        };
        assert_eq!(shape.n_patterns, 4);
    }

    #[test]
    fn plan_advice_default_is_unadvised() {
        let a = PlanAdvice::unadvised();
        assert!(a.estimated_cardinality.is_none());
        assert!(a.suggested_join_order.is_empty());
    }
}
