//! Incremental maintenance (DBSP-style circuit) metrics (SPEC-06). Emitted by
//! `horndb-incremental`: per-tick latency + cardinalities at the tick
//! finalization, and a change-feed subscriber gauge.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct IncrementalMetrics {
    pub tick_duration_seconds: Histogram,
    pub asserted_merged: Counter,
    pub derived_merged: Counter,
    pub closure_withdraw: Counter,
    pub closure_promote: Counter,
    pub fixpoint_rounds: Histogram,
    pub distinct_trace_keys: Gauge,
    pub change_feed_subscribers: Gauge,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

fn count_hist() -> Histogram {
    Histogram::new(exponential_buckets(1.0, 2.0, 10))
}

impl IncrementalMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let tick_duration_seconds = latency_hist();
        let asserted_merged = Counter::default();
        let derived_merged = Counter::default();
        let closure_withdraw = Counter::default();
        let closure_promote = Counter::default();
        let fixpoint_rounds = count_hist();
        let distinct_trace_keys = Gauge::default();
        let change_feed_subscribers = Gauge::default();

        reg.register(
            "incremental_tick_duration_seconds",
            "Incremental tick latency",
            tick_duration_seconds.clone(),
        );
        reg.register(
            "incremental_asserted_merged",
            "Asserted triples merged per tick (total)",
            asserted_merged.clone(),
        );
        reg.register(
            "incremental_derived_merged",
            "Derived triples merged per tick (total)",
            derived_merged.clone(),
        );
        reg.register(
            "incremental_closure_withdraw",
            "Closure triples withdrawn on retract (total)",
            closure_withdraw.clone(),
        );
        reg.register(
            "incremental_closure_promote",
            "Closure triples promoted on retract (total)",
            closure_promote.clone(),
        );
        reg.register(
            "incremental_fixpoint_rounds",
            "Fixpoint rounds run per tick",
            fixpoint_rounds.clone(),
        );
        reg.register(
            "incremental_distinct_trace_keys",
            "Rows in the per-rule weight trace (rule_weights) after the last tick",
            distinct_trace_keys.clone(),
        );
        reg.register(
            "incremental_change_feed_subscribers",
            "Live change-feed subscribers",
            change_feed_subscribers.clone(),
        );

        Self {
            tick_duration_seconds,
            asserted_merged,
            derived_merged,
            closure_withdraw,
            closure_promote,
            fixpoint_rounds,
            distinct_trace_keys,
            change_feed_subscribers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_encodes_incremental_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = IncrementalMetrics::register(&mut reg);
        m.tick_duration_seconds.observe(0.001);
        m.asserted_merged.inc();
        m.fixpoint_rounds.observe(2.0);
        m.distinct_trace_keys.set(3);
        m.change_feed_subscribers.set(1);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains("horndb_incremental_tick_duration_seconds"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_incremental_asserted_merged_total"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_incremental_distinct_trace_keys"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_incremental_change_feed_subscribers"),
            "got:\n{buf}"
        );
    }
}
