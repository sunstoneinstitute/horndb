//! Worst-case-optimal join (leapfrog triejoin) developer metrics (SPEC-03).
//! Emitted by `horndb-wcoj` ONCE per query (on `BatchIter` drop) — never
//! per-seek/per-tuple (§5.3). Counts are accumulated as plain integers in the
//! executor and observed here as per-query distributions.

use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct WcojMetrics {
    pub seeks_per_query: Histogram,
    pub iterations_per_query: Histogram,
    pub peak_iterators: Histogram,
}

impl WcojMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        // wide range for seeks/iterations (1 -> ~4M)
        let seeks_per_query = Histogram::new(exponential_buckets(1.0, 4.0, 12));
        let iterations_per_query = Histogram::new(exponential_buckets(1.0, 4.0, 12));
        // peak iterators ~ number of BGP patterns (small)
        let peak_iterators = Histogram::new(exponential_buckets(1.0, 2.0, 12));

        reg.register(
            "wcoj_seeks_per_query",
            "Trie-iterator seeks per WCOJ query",
            seeks_per_query.clone(),
        );
        reg.register(
            "wcoj_iterations_per_query",
            "Leapfrog convergence iterations per WCOJ query",
            iterations_per_query.clone(),
        );
        reg.register(
            "wcoj_peak_iterators",
            "Active trie iterators per WCOJ query",
            peak_iterators.clone(),
        );

        Self {
            seeks_per_query,
            iterations_per_query,
            peak_iterators,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_encodes_wcoj_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = WcojMetrics::register(&mut reg);
        m.seeks_per_query.observe(7.0);
        m.iterations_per_query.observe(3.0);
        m.peak_iterators.observe(2.0);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_wcoj_seeks_per_query"), "got:\n{buf}");
        assert!(
            buf.contains("horndb_wcoj_iterations_per_query"),
            "got:\n{buf}"
        );
        assert!(buf.contains("horndb_wcoj_peak_iterators"), "got:\n{buf}");
    }
}
