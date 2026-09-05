//! Operator-configuration metrics (SPEC-26 S3/S6). Emitted by
//! `horndb-config`'s live watcher: one counter increment per reload attempt
//! (applied or rejected) plus two gauges describing the config the server is
//! running on right now.
//!
//! The generation gauge is what an operator watches to confirm an edit landed;
//! a `rejected` increment with an unchanged generation says the edit was bad
//! and the previous config is still live.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

use crate::labels::ReloadResultLabel;

#[derive(Clone)]
pub struct ConfigMetrics {
    /// One increment per settled watcher event that ran the reload cycle:
    /// `applied` when the re-resolved config validated and was published,
    /// `rejected` when validation failed and the previous config was kept.
    pub reloads: Family<ReloadResultLabel, Counter>,
    /// Generation of the config currently published. Starts at 1 for the
    /// startup load and increases by one per applied reload.
    pub active_generation: Gauge,
    /// Unix time (seconds) of the most recent applied reload.
    pub last_reload_unixtime: Gauge,
}

impl ConfigMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let reloads = Family::<ReloadResultLabel, Counter>::default();
        let active_generation = Gauge::default();
        let last_reload_unixtime = Gauge::default();

        reg.register(
            "config_reload",
            "Configuration reload attempts by outcome",
            reloads.clone(),
        );
        reg.register(
            "config_active_generation",
            "Generation of the configuration currently in effect",
            active_generation.clone(),
        );
        reg.register(
            "config_last_reload_unixtime",
            "Unix time of the most recent applied configuration reload",
            last_reload_unixtime.clone(),
        );

        Self {
            reloads,
            active_generation,
            last_reload_unixtime,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::ReloadResult;

    #[test]
    fn registers_and_encodes_config_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = ConfigMetrics::register(&mut reg);
        m.reloads
            .get_or_create(&ReloadResultLabel {
                result: ReloadResult::Applied,
            })
            .inc();
        m.reloads
            .get_or_create(&ReloadResultLabel {
                result: ReloadResult::Rejected,
            })
            .inc();
        m.active_generation.set(2);
        m.last_reload_unixtime.set(1_700_000_000);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_config_reload_total{result=\"applied\"}"));
        assert!(buf.contains("horndb_config_reload_total{result=\"rejected\"}"));
        assert!(buf.contains("horndb_config_active_generation 2"));
        assert!(buf.contains("horndb_config_last_reload_unixtime 1700000000"));
    }
}
