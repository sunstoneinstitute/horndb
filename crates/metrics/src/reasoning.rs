//! Named-graph reasoning-view metrics (SPEC-29 P1). Emitted by
//! `horndb-sparql`'s `reasoning::ViewManager` — once per view derivation,
//! plus two gauges the manager republishes whenever the dirty set or the
//! spine version changes. Nothing here is on a query path.
//!
//! No labels: P1 serialises derivations and has at most a few thousand
//! views, so a per-view label would be a high-cardinality series for no
//! operator question these four answer already. Per-view staleness is
//! readable as quads from the view catalog graph instead
//! (`https://horndb.io/graph/views`).

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct ReasoningMetrics {
    /// One increment per view derived. SPEC-29 acceptance 6 reads this to
    /// prove a one-graph update does work in exactly one view.
    pub view_derivations: Counter,
    /// Views currently marked stale and awaiting re-derivation.
    pub views_dirty: Gauge,
    /// The spine version every clean view has closed against. Bumped on any
    /// write to a spine graph.
    pub spine_version: Gauge,
    /// Wall-clock of one view derivation: fork + extend + diff + apply.
    pub derivation_duration_seconds: Histogram,
    /// Wall-clock of one spine template build (the closure computed once per
    /// spine version and shared by every view, SPEC-29 D3).
    pub spine_build_duration_seconds: Histogram,
}

impl ReasoningMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let view_derivations = Counter::default();
        let views_dirty = Gauge::default();
        let spine_version = Gauge::default();
        // 100 µs -> ~5 s, the range a single small-graph derivation lives in
        // (SPEC-06 NF1 budgets 100 ms for single-graph update visibility).
        let derivation_duration_seconds = Histogram::new(exponential_buckets(1e-4, 3.0, 12));
        let spine_build_duration_seconds = Histogram::new(exponential_buckets(1e-3, 3.0, 12));

        reg.register(
            "reasoning_view_derivations",
            "Reasoning view derivations completed",
            view_derivations.clone(),
        );
        reg.register(
            "reasoning_views_dirty",
            "Reasoning views marked stale and awaiting re-derivation",
            views_dirty.clone(),
        );
        reg.register(
            "reasoning_spine_version",
            "Current vocabulary-spine version",
            spine_version.clone(),
        );
        reg.register(
            "reasoning_derivation_duration_seconds",
            "Wall-clock of one reasoning view derivation",
            derivation_duration_seconds.clone(),
        );
        reg.register(
            "reasoning_spine_build_duration_seconds",
            "Wall-clock of one vocabulary-spine template build",
            spine_build_duration_seconds.clone(),
        );

        Self {
            view_derivations,
            views_dirty,
            spine_version,
            derivation_duration_seconds,
            spine_build_duration_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_and_encodes_reasoning_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = ReasoningMetrics::register(&mut reg);
        m.view_derivations.inc();
        m.views_dirty.set(3);
        m.spine_version.set(7);
        m.derivation_duration_seconds.observe(0.01);
        m.spine_build_duration_seconds.observe(0.5);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        for name in [
            "horndb_reasoning_view_derivations_total",
            "horndb_reasoning_views_dirty",
            "horndb_reasoning_spine_version",
            "horndb_reasoning_derivation_duration_seconds",
            "horndb_reasoning_spine_build_duration_seconds",
        ] {
            assert!(buf.contains(name), "missing {name}, got:\n{buf}");
        }
    }
}
