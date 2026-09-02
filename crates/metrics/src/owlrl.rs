//! OWL 2 RL materialization metrics (SPEC-04). Emitted by `horndb-owlrl`:
//! per-rule fire counts and latency at the rule-fire site, and aggregate
//! counters + per-phase latency once per `materialize_with` call. Plus the
//! `reasoning_backend` info gauge, set once by `serve` to name the configured
//! closure backend.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::{PhaseLabel, ReasoningBackend, ReasoningBackendLabel, RuleLabel};

#[derive(Clone)]
pub struct OwlrlMetrics {
    pub rule_fires: Family<RuleLabel, Counter>,
    pub rule_duration_seconds: Family<RuleLabel, Histogram>,
    pub phase_duration_seconds: Family<PhaseLabel, Histogram>,
    pub triples_inferred: Counter,
    pub rounds: Counter,
    pub rule_pruned: Counter,
    pub rule_considered: Counter,
    /// 1 iff the last materialization derived the OWL 2 RL `owl:Nothing`
    /// inconsistency marker (HDB-125). Scraped as
    /// `horndb_reasoning_inconsistent` — deliberately not under the
    /// `owlrl_` prefix: it describes the served closure, not the rule engine.
    pub reasoning_inconsistent: Gauge,
    /// Info gauge: 1 on the series for the closure backend in use.
    pub reasoning_backend: Family<ReasoningBackendLabel, Gauge>,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl OwlrlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let rule_fires = Family::<RuleLabel, Counter>::default();
        let rule_duration_seconds =
            Family::<RuleLabel, Histogram>::new_with_constructor(latency_hist);
        let phase_duration_seconds =
            Family::<PhaseLabel, Histogram>::new_with_constructor(latency_hist);
        let triples_inferred = Counter::default();
        let rounds = Counter::default();
        let rule_pruned = Counter::default();
        let rule_considered = Counter::default();
        let reasoning_inconsistent = Gauge::default();
        let reasoning_backend = Family::<ReasoningBackendLabel, Gauge>::default();

        reg.register(
            "owlrl_rule_fires",
            "OWL RL rule fires by rule id",
            rule_fires.clone(),
        );
        reg.register(
            "owlrl_rule_duration_seconds",
            "OWL RL per-rule fire latency",
            rule_duration_seconds.clone(),
        );
        reg.register(
            "owlrl_phase_duration_seconds",
            "OWL RL per-phase materialize latency",
            phase_duration_seconds.clone(),
        );
        reg.register(
            "owlrl_triples_inferred",
            "Triples inferred by OWL RL materialization",
            triples_inferred.clone(),
        );
        reg.register("owlrl_rounds", "OWL RL semi-naïve rounds", rounds.clone());
        reg.register(
            "owlrl_rule_pruned",
            "OWL RL rule evaluations skipped by the dirty-predicate prune",
            rule_pruned.clone(),
        );
        reg.register(
            "owlrl_rule_considered",
            "OWL RL rule evaluations considered (prune denominator)",
            rule_considered.clone(),
        );
        reg.register(
            "reasoning_backend",
            "Configured OWL RL closure backend (1 on the active backend series)",
            reasoning_backend.clone(),
        );

        reg.register(
            "reasoning_inconsistent",
            "1 iff the materialized closure is OWL 2 RL inconsistent (some individual is an owl:Nothing)",
            reasoning_inconsistent.clone(),
        );

        Self {
            rule_fires,
            rule_duration_seconds,
            phase_duration_seconds,
            triples_inferred,
            rounds,
            rule_pruned,
            rule_considered,
            reasoning_inconsistent,
            reasoning_backend,
        }
    }

    /// Mark `backend` as the closure backend in use by setting its series to 1.
    pub fn record_backend(&self, backend: ReasoningBackend) {
        self.reasoning_backend
            .get_or_create(&ReasoningBackendLabel { backend })
            .set(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::{Phase, PhaseLabel, RuleLabel};

    #[test]
    fn registers_and_encodes_owlrl_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = OwlrlMetrics::register(&mut reg);
        m.rule_fires
            .get_or_create(&RuleLabel {
                rule: "cax-sco".to_string(),
            })
            .inc();
        m.phase_duration_seconds
            .get_or_create(&PhaseLabel {
                phase: Phase::Apply,
            })
            .observe(0.001);
        m.triples_inferred.inc();

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_owlrl_rule_fires_total"), "got:\n{buf}");
        assert!(buf.contains("rule=\"cax-sco\""), "got:\n{buf}");
        assert!(buf.contains("phase=\"apply\""), "got:\n{buf}");
        assert!(
            buf.contains("horndb_owlrl_triples_inferred_total"),
            "got:\n{buf}"
        );
        m.reasoning_inconsistent.set(1);
        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains("horndb_reasoning_inconsistent 1"),
            "got:\n{buf}"
        );
    }

    #[test]
    fn registers_and_encodes_reasoning_backend_gauge() {
        let mut reg = Registry::with_prefix("horndb");
        let m = OwlrlMetrics::register(&mut reg);
        m.record_backend(crate::labels::ReasoningBackend::GraphBlas);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains("horndb_reasoning_backend{backend=\"graphblas\"} 1"),
            "got:\n{buf}"
        );
    }
}
