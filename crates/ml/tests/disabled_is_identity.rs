//! NF1 / acceptance #1: with `ml.enabled = false`, all accessor
//! methods return the canonical `Disabled*` no-ops — proving that
//! downstream callers see identical behaviour to a build with no
//! ML plugins compiled in.
//!
//! This is the test that protects the "ML cannot affect correctness"
//! guarantee at the boundary itself. SPEC-01's conformance harness
//! adds the *engine-wide* version of this check; here we lock the
//! boundary down.

use reasoner_ml::candidate::DisabledCandidateGenerator;
use reasoner_ml::hotset::DisabledHotSetAdvisor;
use reasoner_ml::planner::DisabledPlanAdvisor;
use reasoner_ml::types::{Confidence, PlanAdvice, SubplanShape, TripleSubject};
use reasoner_ml::{MlConfig, MlRegistry};

#[test]
fn disabled_candidate_returns_zero_confidence() {
    let r = MlRegistry::new(MlConfig::disabled());
    let g = r.candidate_generator();
    let a = TripleSubject::Iri("http://x/a".into());
    let b = TripleSubject::Iri("http://x/b".into());
    assert_eq!(g.propose_sameas(&a, &b), Confidence::zero());
    assert_eq!(g.model_id().as_str(), DisabledCandidateGenerator::MODEL_ID);
}

#[test]
fn disabled_planner_returns_unadvised() {
    let r = MlRegistry::new(MlConfig::disabled());
    let p = r.plan_advisor();
    let shape = SubplanShape {
        n_patterns: 4,
        n_vars: 3,
        bound_vars: 1,
    };
    assert_eq!(p.advise(&shape), PlanAdvice::unadvised());
    assert_eq!(p.model_id().as_str(), DisabledPlanAdvisor::MODEL_ID);
}

#[test]
fn disabled_hotset_returns_empty() {
    let r = MlRegistry::new(MlConfig::disabled());
    let h = r.hotset_advisor();
    assert!(h.predict_hot(1000).is_empty());
    assert_eq!(h.model_id().as_str(), DisabledHotSetAdvisor::MODEL_ID);
}

#[test]
fn audit_log_is_empty_under_disabled_config() {
    // The audit log itself exists, but with no plugins firing it
    // stays empty for the lifetime of the registry.
    let r = MlRegistry::new(MlConfig::disabled());
    assert!(r.audit_log().is_empty());
}
