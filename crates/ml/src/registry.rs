//! Central accessor for ML plugins (SPEC-08).
//!
//! The engine asks the registry for a plugin instance; the registry
//! returns either the registered impl or a `Disabled*` no-op,
//! depending on the current [`MlConfig`]. Configuration is
//! hot-reloadable via [`MlRegistry::reload_config`] — acceptance #5.
//!
//! Thread safety: all accessors are read-only on the hot path and
//! held under an `RwLock`. Reloads acquire a write lock for the
//! duration of a single swap.

use crate::audit::MlAuditLog;
use crate::candidate::{CandidateGenerator, DisabledCandidateGenerator};
use crate::config::{LlmPrivacy, MlConfig};
use crate::hotset::{DisabledHotSetAdvisor, HotSetAdvisor};
use crate::nlquery::{DisabledTranslator, Translator};
use crate::planner::{DisabledPlanAdvisor, PlanAdvisor};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Pick the registered plugin when ML is on, else the cached no-op.
/// `Disabled*` fallbacks are also used when ML is on but nothing is
/// registered, so the hot path never allocates.
fn resolve<T: ?Sized>(enabled: bool, registered: &Option<Arc<T>>, disabled: &Arc<T>) -> Arc<T> {
    match registered {
        Some(plugin) if enabled => plugin.clone(),
        _ => disabled.clone(),
    }
}

pub struct MlRegistry {
    inner: RwLock<RegistryInner>,
    audit: Arc<MlAuditLog>,
}

struct RegistryInner {
    config: MlConfig,
    candidate: Option<Arc<dyn CandidateGenerator>>,
    planner: Option<Arc<dyn PlanAdvisor>>,
    hotset: Option<Arc<dyn HotSetAdvisor>>,
    translator: Option<Arc<dyn Translator>>,

    // Cached no-op fallbacks so the disabled hot path returns the
    // same Arc instance every time (no allocation per call).
    disabled_candidate: Arc<dyn CandidateGenerator>,
    disabled_planner: Arc<dyn PlanAdvisor>,
    disabled_hotset: Arc<dyn HotSetAdvisor>,
    disabled_translator: Arc<dyn Translator>,
}

impl MlRegistry {
    pub fn new(config: MlConfig) -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                config,
                candidate: None,
                planner: None,
                hotset: None,
                translator: None,
                disabled_candidate: Arc::new(DisabledCandidateGenerator),
                disabled_planner: Arc::new(DisabledPlanAdvisor),
                disabled_hotset: Arc::new(DisabledHotSetAdvisor),
                disabled_translator: Arc::new(DisabledTranslator),
            }),
            audit: Arc::new(MlAuditLog::new()),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, RegistryInner> {
        self.inner.read().expect("registry rwlock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, RegistryInner> {
        self.inner.write().expect("registry rwlock poisoned")
    }

    pub fn is_enabled(&self) -> bool {
        self.read().config.enabled
    }

    /// Hot-reload the config (acceptance #5 — no restart).
    ///
    /// Switching from enabled to disabled keeps registered plugins
    /// in place but accessor methods return the `Disabled*` no-ops
    /// until re-enabled.
    pub fn reload_config(&self, config: MlConfig) {
        self.write().config = config;
    }

    pub fn register_candidate(&self, g: Arc<dyn CandidateGenerator>) {
        self.write().candidate = Some(g);
    }

    pub fn register_planner(&self, p: Arc<dyn PlanAdvisor>) {
        self.write().planner = Some(p);
    }

    pub fn register_hotset(&self, h: Arc<dyn HotSetAdvisor>) {
        self.write().hotset = Some(h);
    }

    pub fn register_translator(&self, t: Arc<dyn Translator>) {
        self.write().translator = Some(t);
    }

    pub fn candidate_generator(&self) -> Arc<dyn CandidateGenerator> {
        let g = self.read();
        resolve(g.config.enabled, &g.candidate, &g.disabled_candidate)
    }

    pub fn plan_advisor(&self) -> Arc<dyn PlanAdvisor> {
        let g = self.read();
        resolve(g.config.enabled, &g.planner, &g.disabled_planner)
    }

    pub fn hotset_advisor(&self) -> Arc<dyn HotSetAdvisor> {
        let g = self.read();
        resolve(g.config.enabled, &g.hotset, &g.disabled_hotset)
    }

    /// The active NL→SPARQL translator (SPEC-08 F3). Like the other
    /// accessors, routes to the `Disabled*` no-op when ML is off or
    /// nothing is registered — so `/nl-query` fails closed rather than
    /// silently guessing.
    pub fn translator(&self) -> Arc<dyn Translator> {
        let g = self.read();
        resolve(g.config.enabled, &g.translator, &g.disabled_translator)
    }

    /// The current LLM privacy / training-data-leakage policy (F3).
    pub fn llm_privacy(&self) -> LlmPrivacy {
        self.read().config.llm_privacy.clone()
    }

    pub fn audit_log(&self) -> Arc<MlAuditLog> {
        self.audit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelId, TripleSubject};

    #[test]
    fn disabled_returns_no_op_candidate() {
        let r = MlRegistry::new(MlConfig::disabled());
        let g = r.candidate_generator();
        assert_eq!(g.model_id().as_str(), DisabledCandidateGenerator::MODEL_ID);
    }

    #[test]
    fn enabled_without_registration_returns_no_op() {
        let r = MlRegistry::new(MlConfig::enabled());
        let g = r.candidate_generator();
        // Enabled but nothing registered: still no-op.
        assert_eq!(g.model_id().as_str(), DisabledCandidateGenerator::MODEL_ID);
    }

    struct FakeCandidate;
    impl CandidateGenerator for FakeCandidate {
        fn model_id(&self) -> ModelId {
            ModelId::new("fake")
        }
        fn propose_sameas(
            &self,
            _left: &TripleSubject,
            _right: &TripleSubject,
        ) -> crate::types::Confidence {
            crate::types::Confidence::new(0.99)
        }
    }

    #[test]
    fn enabled_with_registered_returns_registered() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        let g = r.candidate_generator();
        assert_eq!(g.model_id().as_str(), "fake");
    }

    #[test]
    fn registered_but_disabled_returns_no_op() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        r.reload_config(MlConfig::disabled());
        let g = r.candidate_generator();
        // The registered plugin is still in the registry, but the
        // config switch routes us back to the no-op.
        assert_eq!(g.model_id().as_str(), DisabledCandidateGenerator::MODEL_ID);
    }

    #[test]
    fn re_enable_restores_registered() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        r.reload_config(MlConfig::disabled());
        r.reload_config(MlConfig::enabled());
        assert_eq!(r.candidate_generator().model_id().as_str(), "fake");
    }

    #[test]
    fn disabled_returns_no_op_translator() {
        let r = MlRegistry::new(MlConfig::disabled());
        assert_eq!(
            r.translator().model_id().as_str(),
            crate::nlquery::DisabledTranslator::MODEL_ID
        );
    }

    #[test]
    fn enabled_with_registered_translator_returns_it() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_translator(Arc::new(crate::nlquery::MockTranslator::new(
            "mock-v1",
            "SELECT * WHERE { ?s ?p ?o }",
        )));
        assert_eq!(r.translator().model_id().as_str(), "mock-v1");
    }

    #[test]
    fn registered_translator_but_disabled_returns_no_op() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_translator(Arc::new(crate::nlquery::MockTranslator::new(
            "mock-v1",
            "SELECT * WHERE { ?s ?p ?o }",
        )));
        r.reload_config(MlConfig::disabled());
        assert_eq!(
            r.translator().model_id().as_str(),
            crate::nlquery::DisabledTranslator::MODEL_ID
        );
    }

    #[test]
    fn privacy_reflects_config() {
        let r = MlRegistry::new(
            MlConfig::enabled().with_privacy(crate::config::LlmPrivacy::retain_questions()),
        );
        assert!(r.llm_privacy().log_questions);
    }
}
