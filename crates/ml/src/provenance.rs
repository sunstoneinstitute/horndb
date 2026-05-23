//! Provenance annotation attached to ML-derived triples (SPEC-08 F5).
//!
//! Storage (SPEC-02) is expected to materialize this as an optional
//! column on the triple partition; this crate owns the schema.

use crate::types::{Confidence, ModelId};

/// Where a triple came from.
///
/// `Symbolic` is the default for triples derived by rule firing or
/// closure; SPARQL planners (SPEC-07) can filter on this for audit.
#[derive(Debug, Clone, PartialEq)]
pub enum MlProvenance {
    Symbolic,
    MlDerived {
        model: ModelId,
        confidence: Confidence,
    },
}

impl MlProvenance {
    pub fn is_ml_derived(&self) -> bool {
        matches!(self, MlProvenance::MlDerived { .. })
    }

    /// Discriminant byte used by SPEC-02 when packing the provenance
    /// column. Stable across crate versions — appending a new variant
    /// must keep existing bytes intact.
    pub const SYMBOLIC_TAG: u8 = 0x00;
    pub const ML_DERIVED_TAG: u8 = 0x01;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_is_not_ml_derived() {
        assert!(!MlProvenance::Symbolic.is_ml_derived());
    }

    #[test]
    fn ml_derived_is_ml_derived() {
        let p = MlProvenance::MlDerived {
            model: ModelId::new("test-model"),
            confidence: Confidence::new(0.42),
        };
        assert!(p.is_ml_derived());
    }

    #[test]
    fn tag_bytes_are_stable() {
        // SPEC-02 will pack these into a storage column — must be stable.
        assert_eq!(MlProvenance::SYMBOLIC_TAG, 0x00);
        assert_eq!(MlProvenance::ML_DERIVED_TAG, 0x01);
    }
}
