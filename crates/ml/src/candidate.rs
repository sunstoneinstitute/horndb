//! Candidate-link generation (SPEC-08 F1).
//!
//! ML systems propose candidate `owl:sameAs` links between subjects.
//! Every proposal is a *hypothesis* — the engine must re-verify
//! symbolically before committing. This crate ships only the trait
//! and a no-op implementation; real implementations (e.g. FAISS) are
//! Stage 2 deliverables.

use crate::types::{Confidence, ModelId, TripleSubject};

pub trait CandidateGenerator: Send + Sync {
    /// Identity of the underlying model (stable across calls).
    fn model_id(&self) -> ModelId;

    /// Propose how confident we are that `left` and `right` denote
    /// the same entity. `Confidence::zero()` means "no opinion."
    fn propose_sameas(&self, left: &TripleSubject, right: &TripleSubject) -> Confidence;
}

/// No-op implementation used when ML is disabled (NF1).
///
/// Always returns `Confidence::zero()` — the engine will never act on
/// its proposals, and re-verification trivially rejects.
#[derive(Debug, Default)]
pub struct DisabledCandidateGenerator;

impl DisabledCandidateGenerator {
    pub const MODEL_ID: &'static str = "disabled-candidate-generator";
}

impl CandidateGenerator for DisabledCandidateGenerator {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }

    fn propose_sameas(&self, _left: &TripleSubject, _right: &TripleSubject) -> Confidence {
        Confidence::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_zero_for_any_pair() {
        let g = DisabledCandidateGenerator;
        let a = TripleSubject::Iri("http://x/a".into());
        let b = TripleSubject::Iri("http://x/b".into());
        assert_eq!(g.propose_sameas(&a, &b).value(), 0.0);
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        let g = DisabledCandidateGenerator;
        assert_eq!(g.model_id().as_str(), "disabled-candidate-generator");
    }

    /// Marker test — the trait must be object-safe so we can store
    /// `Arc<dyn CandidateGenerator>` in the registry (Task 9).
    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn CandidateGenerator> = Box::new(DisabledCandidateGenerator);
    }
}
