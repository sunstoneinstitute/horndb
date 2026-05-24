//! Acceptance #5: enabling/disabling ML via configuration reload
//! requires no engine restart. We simulate the "engine" by stashing
//! the registry in an `Arc` and calling accessors before and after
//! reload from a second thread, confirming the post-reload
//! behaviour without recreating any state.

use horndb_ml::candidate::{CandidateGenerator, DisabledCandidateGenerator};
use horndb_ml::types::{Confidence, ModelId, TripleSubject};
use horndb_ml::{MlConfig, MlRegistry};
use std::sync::Arc;
use std::thread;

struct AlwaysHigh;
impl CandidateGenerator for AlwaysHigh {
    fn model_id(&self) -> ModelId {
        ModelId::new("always-high")
    }
    fn propose_sameas(&self, _left: &TripleSubject, _right: &TripleSubject) -> Confidence {
        Confidence::new(0.99)
    }
}

#[test]
fn hot_reload_round_trip_without_restart() {
    let r = Arc::new(MlRegistry::new(MlConfig::enabled()));
    r.register_candidate(Arc::new(AlwaysHigh));

    // Initially enabled: registered plugin is in effect.
    let a = TripleSubject::Iri("http://x/a".into());
    let b = TripleSubject::Iri("http://x/b".into());
    assert_eq!(
        r.candidate_generator().propose_sameas(&a, &b),
        Confidence::new(0.99)
    );

    // Disable from a worker thread — same registry instance.
    {
        let r2 = r.clone();
        thread::spawn(move || r2.reload_config(MlConfig::disabled()))
            .join()
            .unwrap();
    }
    assert!(!r.is_enabled());
    assert_eq!(
        r.candidate_generator().model_id().as_str(),
        DisabledCandidateGenerator::MODEL_ID
    );

    // Re-enable — registered plugin comes back without re-registration.
    r.reload_config(MlConfig::enabled());
    assert_eq!(
        r.candidate_generator().propose_sameas(&a, &b),
        Confidence::new(0.99)
    );
}
