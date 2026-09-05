//! SPEC-30 §S6 — the applied-position slot's observability surface. Emitted
//! by `crates/sparql/src/feed.rs` (the slot-advance path) and
//! `crates/sparql/src/bin/serve.rs` (startup, once).

use crate::labels::FeedOpLabel;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct FeedMetrics {
    pub applied_batches: Counter,
    pub applied_quads: Family<FeedOpLabel, Counter>,
    pub last_apply_seconds: Gauge<f64, std::sync::atomic::AtomicU64>,
    /// P1 pins this at 0 forever — the rebuild reset that increments it is
    /// P2 (out of this plan's scope). Registered now so a consumer parses
    /// one metric shape across P1/P2/P3.
    pub generation: Gauge,
    /// P1 pins this at 0 forever — no rebuild exists yet to be in progress.
    pub rebuild_in_progress: Gauge,
    /// Set once at startup: 0 when no slot was recovered, which on the P1
    /// (fully in-memory) store is always. Exists so the contract's
    /// observability is in place before P3/P4 durability makes it non-trivial.
    pub recovery_gap_seconds: Gauge,
}

impl FeedMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let applied_batches = Counter::default();
        let applied_quads = Family::<FeedOpLabel, Counter>::default();
        let last_apply_seconds = Gauge::<f64, std::sync::atomic::AtomicU64>::default();
        let generation = Gauge::default();
        let rebuild_in_progress = Gauge::default();
        let recovery_gap_seconds = Gauge::default();

        reg.register(
            "feed_applied_batches",
            "Applied-position slot advances (SPEC-30 S5: one per request that carried a feed position)",
            applied_batches.clone(),
        );
        reg.register(
            "feed_applied_quads",
            "Slot quads written per advance, split by del/add",
            applied_quads.clone(),
        );
        reg.register(
            "feed_last_apply_seconds",
            "Wall-clock cost of the most recent slot advance",
            last_apply_seconds.clone(),
        );
        reg.register(
            "feed_generation",
            "The slot's generation counter (P1: always 0 — incremented by the P2 rebuild reset)",
            generation.clone(),
        );
        reg.register(
            "feed_rebuild_in_progress",
            "1 while a rebuild-from-zero is in progress (P1: always 0 — no rebuild exists yet)",
            rebuild_in_progress.clone(),
        );
        reg.register(
            "feed_recovery_gap_seconds",
            "Set once at startup: 0 when no slot was recovered (always, on the P1 in-memory store)",
            recovery_gap_seconds.clone(),
        );

        Self {
            applied_batches,
            applied_quads,
            last_apply_seconds,
            generation,
            rebuild_in_progress,
            recovery_gap_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::FeedOp;

    #[test]
    fn registers_and_encodes_feed_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = FeedMetrics::register(&mut reg);
        m.applied_batches.inc();
        m.applied_quads
            .get_or_create(&FeedOpLabel { op: FeedOp::Add })
            .inc_by(4);
        m.applied_quads
            .get_or_create(&FeedOpLabel { op: FeedOp::Del })
            .inc_by(4);
        m.last_apply_seconds.set(0.001);
        m.generation.set(0);
        m.rebuild_in_progress.set(0);
        m.recovery_gap_seconds.set(0);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains("horndb_feed_applied_batches_total"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_feed_applied_quads_total"),
            "got:\n{buf}"
        );
        assert!(buf.contains("op=\"add\""), "got:\n{buf}");
        assert!(buf.contains("op=\"del\""), "got:\n{buf}");
        assert!(
            buf.contains("horndb_feed_last_apply_seconds"),
            "got:\n{buf}"
        );
        assert!(buf.contains("horndb_feed_generation"), "got:\n{buf}");
        assert!(
            buf.contains("horndb_feed_rebuild_in_progress"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_feed_recovery_gap_seconds"),
            "got:\n{buf}"
        );
    }
}
