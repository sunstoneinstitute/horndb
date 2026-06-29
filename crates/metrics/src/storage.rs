//! Storage metrics. Load-path counters here; expensive size quantities
//! (triple/graph/predicate counts, dictionary size, tier bytes) are read at
//! SCRAPE TIME via [`StorageCollector`], which reads a cheap stats snapshot
//! through a closure the server installs over a `Weak` ref to the live store.
//! Steady-state cost is therefore zero.
use crate::labels::{MemTier, TierLabel};
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::ConstGauge;
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

/// A cheap, O(1)-ish point-in-time snapshot of store size quantities, read at
/// scrape time. "Cheap" means bounded by the number of distinct predicates /
/// graphs — never an O(triples) traversal.
#[derive(Clone, Copy, Default)]
pub struct StorageSnapshot {
    pub triples: i64,
    pub graphs: i64,
    pub predicates: i64,
    pub dictionary_terms: i64,
    pub tier_bytes_estimated: i64,
}

/// Scrape-time collector that emits the five storage size gauges. It holds a
/// closure that reads a [`StorageSnapshot`] from the live store (typically by
/// upgrading a `Weak` ref); when the closure returns `None` (store gone) the
/// gauges report zero.
pub struct StorageCollector {
    f: Box<dyn Fn() -> Option<StorageSnapshot> + Send + Sync>,
}

impl StorageCollector {
    pub fn new(f: impl Fn() -> Option<StorageSnapshot> + Send + Sync + 'static) -> Self {
        Self { f: Box::new(f) }
    }
}

impl std::fmt::Debug for StorageCollector {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.write_str("StorageCollector")
    }
}

impl Collector for StorageCollector {
    fn encode(&self, mut enc: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let snap = (self.f)().unwrap_or_default();
        for (name, help, val) in [
            ("storage_triples", "Live triples in the store", snap.triples),
            ("storage_graphs", "Distinct named graphs", snap.graphs),
            ("storage_predicates", "Distinct predicates", snap.predicates),
            (
                "storage_dictionary_terms",
                "Interned dictionary terms",
                snap.dictionary_terms,
            ),
        ] {
            let g = ConstGauge::new(val);
            let me = enc.encode_descriptor(name, help, None, g.metric_type())?;
            g.encode(me)?;
        }
        {
            let g = ConstGauge::new(snap.tier_bytes_estimated);
            let mut me = enc.encode_descriptor(
                "storage_tier_bytes_estimated",
                "Estimated tier bytes",
                None,
                g.metric_type(),
            )?;
            let sub = me.encode_family(&TierLabel {
                tier: MemTier::Unknown,
            })?;
            g.encode(sub)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus_client::registry::Registry;

    #[test]
    fn collector_emits_storage_gauges() {
        let mut reg = Registry::with_prefix("horndb");
        reg.register_collector(Box::new(StorageCollector::new(|| {
            Some(StorageSnapshot {
                triples: 42,
                graphs: 1,
                predicates: 3,
                dictionary_terms: 99,
                tier_bytes_estimated: 1024,
            })
        })));
        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_storage_triples 42"), "got:\n{buf}");
        assert!(
            buf.contains("horndb_storage_dictionary_terms 99"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_storage_tier_bytes_estimated{tier=\"unknown\"}"),
            "got:\n{buf}"
        );
    }
}
