//! Storage metrics. Load-path counters here; size gauges via a scrape-time
//! collector added in a later task.
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct StorageMetrics {
    pub load_duration_seconds: Histogram,
    pub load_bytes: Counter,
}

impl StorageMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let load_duration_seconds = Histogram::new(exponential_buckets(1e-3, 3.0, 12));
        let load_bytes = Counter::default();
        reg.register(
            "storage_load_duration_seconds",
            "RDF load duration",
            load_duration_seconds.clone(),
        );
        reg.register(
            "storage_load_bytes",
            "Bytes read during RDF load",
            load_bytes.clone(),
        );
        Self {
            load_duration_seconds,
            load_bytes,
        }
    }
}
