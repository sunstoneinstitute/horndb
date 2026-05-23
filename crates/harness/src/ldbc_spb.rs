//! LDBC SPB driver integration shim (SPEC-01 F4).
//!
//! The LDBC SPB v2.0 driver is a Java program shipped by LDBC. We
//! invoke it as a subprocess and parse its result JSON into the
//! harness metric DB so SPB and our W3C runs live in the same store.
//!
//! Stage-1 scope: SPB-256 (SF=0.256, ~256M triples). SF3/SF5 are
//! Stage-2.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::db::Db;

#[derive(Debug, Deserialize)]
pub struct SpbResult {
    pub editorial_qps: f64,
    pub aggregation_qps: f64,
    pub update_qps: f64,
    pub run_duration_seconds: f64,
}

pub struct SpbConfig<'a> {
    /// Path to the LDBC SPB driver JAR.
    pub driver_jar: &'a Path,
    /// Path to the SPB scenario configuration (test.properties).
    pub scenario: &'a Path,
    /// Endpoint URL of the engine under test.
    pub endpoint: &'a str,
    /// Run duration. Stage-1 default is 600 seconds (10 min) of
    /// measurement — well below an audit-grade 1-hour run but enough
    /// to compare against GraphDB Free for the go/no-go decision.
    pub duration_seconds: u64,
}

pub fn run(cfg: &SpbConfig<'_>) -> Result<SpbResult> {
    let output = Command::new("java")
        .arg("-jar")
        .arg(cfg.driver_jar)
        .arg("--config")
        .arg(cfg.scenario)
        .arg("--endpoint")
        .arg(cfg.endpoint)
        .arg("--duration")
        .arg(cfg.duration_seconds.to_string())
        .arg("--report-format")
        .arg("json")
        .output()
        .with_context(|| "invoking LDBC SPB driver")?;
    if !output.status.success() {
        return Err(anyhow!(
            "SPB driver exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: SpbResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("parsing SPB JSON: {e}"))?;
    Ok(parsed)
}

pub fn record(db: &Db, run_id: &str, reasoner_name: &str, r: &SpbResult) -> Result<()> {
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "editorial-qps", r.editorial_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "aggregation-qps", r.aggregation_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "update-qps", r.update_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "duration-s", r.run_duration_seconds, "s")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spb_result_json() {
        let json = r#"{
            "editorial_qps": 123.4,
            "aggregation_qps": 5.6,
            "update_qps": 78.9,
            "run_duration_seconds": 600.0
        }"#;
        let r: SpbResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.editorial_qps, 123.4);
    }
}
