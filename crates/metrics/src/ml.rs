//! ML/LLM boundary metrics (SPEC-08). Emitted by `horndb-ml`'s server module
//! (behind the `server` feature): NL-query results, LLM token/cost usage, and
//! translate/execute/audit-query latency.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::NlResultLabel;

#[derive(Clone)]
pub struct MlMetrics {
    pub nl_query: Family<NlResultLabel, Counter>,
    pub prompt_tokens: Counter,
    pub completion_tokens: Counter,
    pub estimated_usd: Counter<f64>,
    pub translate_duration_seconds: Histogram,
    pub execute_duration_seconds: Histogram,
    pub audit_query_duration_seconds: Histogram,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl MlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let nl_query = Family::<NlResultLabel, Counter>::default();
        let prompt_tokens = Counter::default();
        let completion_tokens = Counter::default();
        let estimated_usd = Counter::<f64>::default();
        let translate_duration_seconds = latency_hist();
        let execute_duration_seconds = latency_hist();
        let audit_query_duration_seconds = latency_hist();

        reg.register("ml_nl_query", "NL queries by result", nl_query.clone());
        reg.register(
            "ml_prompt_tokens",
            "LLM prompt tokens consumed",
            prompt_tokens.clone(),
        );
        reg.register(
            "ml_completion_tokens",
            "LLM completion tokens produced",
            completion_tokens.clone(),
        );
        reg.register(
            "ml_estimated_usd",
            "Estimated LLM spend (USD)",
            estimated_usd.clone(),
        );
        reg.register(
            "ml_translate_duration_seconds",
            "NL->SPARQL translate latency",
            translate_duration_seconds.clone(),
        );
        reg.register(
            "ml_execute_duration_seconds",
            "Translated-query execute latency",
            execute_duration_seconds.clone(),
        );
        reg.register(
            "ml_audit_query_duration_seconds",
            "ML audit-log query latency",
            audit_query_duration_seconds.clone(),
        );

        Self {
            nl_query,
            prompt_tokens,
            completion_tokens,
            estimated_usd,
            translate_duration_seconds,
            execute_duration_seconds,
            audit_query_duration_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::{NlResult, NlResultLabel};

    #[test]
    fn registers_and_encodes_ml_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = MlMetrics::register(&mut reg);
        m.nl_query
            .get_or_create(&NlResultLabel {
                result: NlResult::Ok,
            })
            .inc();
        m.prompt_tokens.inc_by(10);
        m.estimated_usd.inc_by(0.002);
        m.translate_duration_seconds.observe(0.01);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_ml_nl_query_total"), "got:\n{buf}");
        assert!(buf.contains("result=\"ok\""), "got:\n{buf}");
        assert!(buf.contains("horndb_ml_prompt_tokens_total"), "got:\n{buf}");
        assert!(buf.contains("horndb_ml_estimated_usd_total"), "got:\n{buf}");
        assert!(
            buf.contains("horndb_ml_translate_duration_seconds"),
            "got:\n{buf}"
        );
    }
}
