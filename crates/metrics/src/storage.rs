//! Storage metrics. Load-path counters here; expensive size quantities
//! (triple/graph/predicate counts, dictionary size, tier bytes) are read at
//! SCRAPE TIME via [`StorageCollector`], which reads a stats snapshot through a
//! closure the server installs over a `Weak` ref to the live store. Nothing is
//! paid between scrapes. What a scrape itself costs is no longer always small —
//! see [`StorageSnapshot`].
use crate::labels::{
    LoadPhase, LoadPhaseLabel, MemTier, MergeTrigger, MergeTriggerLabel, TierLabel,
};
use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::time::Duration;

#[derive(Clone)]
pub struct StorageMetrics {
    pub load_duration_seconds: Histogram,
    pub load_bytes: Counter,
    /// Nanoseconds spent in each bulk-load phase, and rows each phase handled.
    /// A count+sum pair per SPEC-17 §5.4.1: the mean phase cost per row is
    /// `rate(nanoseconds) / rate(rows)`. Written once per phase per batch by
    /// [`StorageMetrics::record_load_phase`], never from inside a loop.
    pub load_phase_nanoseconds: Family<LoadPhaseLabel, Counter>,
    pub load_phase_rows: Family<LoadPhaseLabel, Counter>,
    /// Wall-clock of one partition run merge, and a count of merges by what
    /// triggered them (HDB-122). The `load_phase_*` pair above sums the same
    /// work; this is the *distribution*, which is what a tail-latency question
    /// needs — a p99 of seconds is invisible in a sum.
    pub partition_merge_seconds: Histogram,
    pub partition_merges: Family<MergeTriggerLabel, Counter>,
}

impl StorageMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let load_duration_seconds = Histogram::new(exponential_buckets(1e-3, 3.0, 12));
        let load_bytes = Counter::default();
        let load_phase_nanoseconds = Family::<LoadPhaseLabel, Counter>::default();
        let load_phase_rows = Family::<LoadPhaseLabel, Counter>::default();
        // 100 us to ~4.9 h: a small partition merges in microseconds, a 10M-row
        // one in seconds.
        let partition_merge_seconds = Histogram::new(exponential_buckets(1e-4, 4.0, 12));
        let partition_merges = Family::<MergeTriggerLabel, Counter>::default();
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
        reg.register(
            "storage_load_phase_nanoseconds",
            "Nanoseconds spent in each bulk-load phase",
            load_phase_nanoseconds.clone(),
        );
        reg.register(
            "storage_load_phase_rows",
            "Rows handled by each bulk-load phase",
            load_phase_rows.clone(),
        );
        reg.register(
            "storage_partition_merge_seconds",
            "Duration of one partition run merge",
            partition_merge_seconds.clone(),
        );
        reg.register(
            "storage_partition_merges",
            "Partition run merges, by what triggered them",
            partition_merges.clone(),
        );
        Self {
            load_duration_seconds,
            load_bytes,
            load_phase_nanoseconds,
            load_phase_rows,
            partition_merge_seconds,
            partition_merges,
        }
    }

    /// Record one partition run merge: how long it took and what triggered it.
    pub fn record_partition_merge(&self, trigger: MergeTrigger, elapsed: Duration) {
        self.partition_merge_seconds.observe(elapsed.as_secs_f64());
        self.partition_merges
            .get_or_create(&MergeTriggerLabel { trigger })
            .inc();
    }

    /// Record one bulk-load phase: its elapsed time and the rows it handled.
    ///
    /// Call this **once per phase per batch**, after the loop has finished —
    /// never per row (SPEC-17 §5.4). The caller accumulates in locals and takes
    /// one `Instant::now()` on each side of the loop; this is the single point
    /// where a shared metric handle is touched.
    pub fn record_load_phase(&self, phase: LoadPhase, elapsed: Duration, rows: u64) {
        let label = LoadPhaseLabel { phase };
        self.load_phase_nanoseconds
            .get_or_create(&label)
            .inc_by(elapsed.as_nanos() as u64);
        self.load_phase_rows.get_or_create(&label).inc_by(rows);
    }
}

/// A point-in-time snapshot of store size quantities, read at scrape time.
///
/// Normally bounded by the number of distinct predicates / graphs. **One
/// exception, since HDB-84:** reading a partition that has been written in
/// batches merges its runs first, which is O(rows in that partition). A scrape
/// is a read like any other, so a scrape that lands after a bulk load — or
/// after any batched write nothing has read yet — pays that merge, once per
/// affected partition. On a 10M-triple store that is order of a second. Every
/// later scrape is bounded again.
///
/// The server's collector reads this under the store's read guard, held for the
/// whole snapshot, so that first scrape stalls **every** reader and writer of
/// the store, not just the partitions being merged.
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
