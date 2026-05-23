//! Planner-advice plugin (SPEC-08 F2).
//!
//! The planner (SPEC-03 / SPEC-07) consults this for cardinality
//! hints and join-order suggestions but always validates against
//! its own histograms and falls back if the advice is implausible.

use crate::types::{ModelId, PlanAdvice, SubplanShape};

pub trait PlanAdvisor: Send + Sync {
    fn model_id(&self) -> ModelId;
    fn advise(&self, shape: &SubplanShape) -> PlanAdvice;
}

/// No-op implementation used when ML is disabled.
///
/// Returns `PlanAdvice::unadvised()` — the planner uses its own
/// histograms exclusively.
#[derive(Debug, Default)]
pub struct DisabledPlanAdvisor;

impl DisabledPlanAdvisor {
    pub const MODEL_ID: &'static str = "disabled-plan-advisor";
}

impl PlanAdvisor for DisabledPlanAdvisor {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }
    fn advise(&self, _shape: &SubplanShape) -> PlanAdvice {
        PlanAdvice::unadvised()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_unadvised() {
        let a = DisabledPlanAdvisor.advise(&SubplanShape {
            n_patterns: 5,
            n_vars: 4,
            bound_vars: 2,
        });
        assert_eq!(a, PlanAdvice::unadvised());
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        assert_eq!(
            DisabledPlanAdvisor.model_id().as_str(),
            "disabled-plan-advisor"
        );
    }

    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn PlanAdvisor> = Box::new(DisabledPlanAdvisor);
    }
}
