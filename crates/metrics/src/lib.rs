//! HornDB metrics: prometheus-client registry with typed labels.
//!
//! A process-global `MetricsState` (init-once via `OnceLock`) owns the
//! `Registry` and all typed metric handles. Hot-path code reaches a handle
//! through `metrics()`; expensive sizes are read at scrape time via collectors
//! registered with `register_collector`.

pub mod closure;
pub mod incremental;
pub mod labels;
pub mod ml;
pub mod owlrl;
pub mod sparql;
pub mod storage;
pub mod wcoj;

use prometheus_client::collector::Collector;
use prometheus_client::registry::Registry;
use std::sync::{Mutex, OnceLock};

pub struct MetricsState {
    registry: Mutex<Registry>,
    pub sparql: sparql::SparqlMetrics,
    pub closure: closure::ClosureSink,
    pub storage: storage::StorageMetrics,
    pub owlrl: owlrl::OwlrlMetrics,
    pub incremental: incremental::IncrementalMetrics,
    pub ml: ml::MlMetrics,
    pub wcoj: wcoj::WcojMetrics,
}

impl MetricsState {
    pub fn new() -> Self {
        let mut registry = Registry::with_prefix("horndb");
        let sparql = sparql::SparqlMetrics::register(&mut registry);
        let closure = closure::ClosureSink::register(&mut registry);
        let storage = storage::StorageMetrics::register(&mut registry);
        let owlrl = owlrl::OwlrlMetrics::register(&mut registry);
        let incremental = incremental::IncrementalMetrics::register(&mut registry);
        let ml = ml::MlMetrics::register(&mut registry);
        let wcoj = wcoj::WcojMetrics::register(&mut registry);
        Self {
            registry: Mutex::new(registry),
            sparql,
            closure,
            storage,
            owlrl,
            incremental,
            ml,
            wcoj,
        }
    }

    pub fn encode(&self) -> String {
        let mut buf = String::new();
        let reg = self.registry.lock().expect("metrics registry poisoned");
        prometheus_client::encoding::text::encode(&mut buf, &reg)
            .expect("encode into String is infallible");
        buf
    }

    pub fn register_collector(&self, c: Box<dyn Collector>) {
        self.registry
            .lock()
            .expect("metrics registry poisoned")
            .register_collector(c);
    }
}

impl Default for MetricsState {
    fn default() -> Self {
        Self::new()
    }
}

static METRICS: OnceLock<MetricsState> = OnceLock::new();

pub fn metrics() -> &'static MetricsState {
    METRICS.get_or_init(MetricsState::new)
}

pub fn encode_metrics() -> String {
    metrics().encode()
}

pub fn register_collector(c: Box<dyn Collector>) {
    metrics().register_collector(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_contains_registered_metric() {
        let state = MetricsState::new();
        state
            .sparql
            .requests
            .get_or_create(&crate::labels::RequestLabels {
                endpoint: crate::labels::Endpoint::Query,
                method: crate::labels::Method::Get,
                status: 200,
            })
            .inc();
        let out = state.encode();
        assert!(out.contains("horndb_sparql_requests_total"));
        assert!(out.contains("endpoint=\"query\""));
    }
}
