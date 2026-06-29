//! OWL 2 RL materialization metrics (SPEC-04). Emitted by `horndb-owlrl`:
//! per-rule fire counts and latency at the rule-fire site, and aggregate
//! counters + per-phase latency once per `materialize_with` call.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::{PhaseLabel, RuleLabel};

#[derive(Clone)]
pub struct OwlrlMetrics {
    pub rule_fires: Family<RuleLabel, Counter>,
    pub rule_duration_seconds: Family<RuleLabel, Histogram>,
    pub phase_duration_seconds: Family<PhaseLabel, Histogram>,
    pub triples_inferred: Counter,
    pub rounds: Counter,
    pub rule_pruned: Counter,
    pub rule_considered: Counter,
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

        Self {
            rule_fires,
            rule_duration_seconds,
            phase_duration_seconds,
            triples_inferred,
            rounds,
            rule_pruned,
            rule_considered,
        }
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
    }
}
