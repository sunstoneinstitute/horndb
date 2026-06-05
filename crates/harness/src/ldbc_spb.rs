//! LDBC SPB driver integration shim (SPEC-01 F4).
//!
//! The LDBC SPB v2.0 driver is a Java program shipped by LDBC. We
//! invoke it as a subprocess and parse its result report into the
//! harness metric DB so SPB and our W3C runs live in the same store.
//!
//! ## How the upstream driver is actually invoked
//!
//! Contrary to a CLI-flags model, the SPB v2.0 `TestDriver` takes a
//! **single positional argument** — the path to a `.properties`
//! scenario file (`TestDriver.main` rejects anything other than
//! `args.length == 1`). Everything else — endpoint URLs, run duration,
//! agent counts, dataset paths, which operational phases to run — is
//! read from that file. There are no `--endpoint` / `--duration` /
//! `--report-format` flags, and the driver emits a **human-readable
//! text report** to stdout, not JSON.
//!
//! So this shim works by *merging* the harness-level knobs (endpoint,
//! duration) on top of the caller-supplied scenario file, writing the
//! merged result to a temp file, and handing that single path to the
//! driver. The scenario file remains the source of truth for the
//! operational phases (it must have `runBenchmark=true` and the
//! load/generate phases off — the store is expected to be pre-loaded
//! by a separate bootstrap step) and for any engine-specific paths.
//!
//! ## What we parse back out
//!
//! The driver's reporter prints a cumulative summary block roughly
//! once per second; the final block before shutdown carries the run's
//! headline averages. We scrape the **last** occurrence of:
//!   - `N.NNNN average operations per second` → editorial throughput
//!     (CW inserts + updates + deletes combined — SPB does not break
//!     out a separate "update QPS" in the summary)
//!   - `N.NNNN average queries per second`    → aggregation throughput
//!   - `Seconds : N`                          → measured run duration
//!
//! Stage-1 scope: SPB-256 (SF=0.256, ~256M triples). SF3/SF5 are
//! Stage-2.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::db::Db;

/// Headline throughput numbers scraped from an SPB driver run.
///
/// These mirror what the upstream reporter actually emits — editorial
/// *operations* per second and aggregation *queries* per second — plus
/// the measured run duration. SPB does not report a standalone update
/// rate in its summary, so there is no `update_qps` field.
#[derive(Debug, Clone, PartialEq)]
pub struct SpbResult {
    /// "average operations per second" — editorial agents (CW insert /
    /// update / delete, combined).
    pub editorial_ops_per_sec: f64,
    /// "average queries per second" — aggregation agents.
    pub aggregation_queries_per_sec: f64,
    /// "Seconds : N" — wall-clock measurement window the averages were
    /// taken over.
    pub run_duration_seconds: f64,
}

pub struct SpbConfig<'a> {
    /// Path to the LDBC SPB driver JAR.
    pub driver_jar: &'a Path,
    /// Path to the base SPB scenario configuration (`.properties`).
    /// Carries the operational phases and engine-specific paths; the
    /// endpoint and duration below are merged on top of it.
    pub scenario: &'a Path,
    /// SPARQL **query** endpoint of the engine under test. Overrides
    /// `endpointURL` in the scenario file.
    pub endpoint: &'a str,
    /// SPARQL **update** endpoint (editorial agents POST here).
    /// Overrides `endpointUpdateURL`. If `None`, the query endpoint is
    /// reused — correct for engines (e.g. RDFox) that accept update at
    /// the same path; set it explicitly for engines that split them.
    pub endpoint_update: Option<&'a str>,
    /// Run duration. Overrides `benchmarkRunPeriodSeconds`. Stage-1
    /// default is 600 seconds (10 min) of measurement — well below an
    /// audit-grade 1-hour run but enough to compare against GraphDB
    /// Free for the go/no-go decision.
    pub duration_seconds: u64,
}

pub fn run(cfg: &SpbConfig<'_>) -> Result<SpbResult> {
    if !cfg.driver_jar.is_file() {
        bail!(
            "SPB driver JAR not found at {} — build it first \
             (cd into the SPB checkout and run `ant build-basic-querymix`), \
             or point --driver-jar / SPB_DRIVER_JAR at the built jar",
            cfg.driver_jar.display()
        );
    }
    // Resolve the jar to an absolute path: we run the driver with a
    // different working directory (the scenario's dir, see below), so a
    // relative `--driver-jar` would otherwise fail to resolve.
    let driver_jar = cfg
        .driver_jar
        .canonicalize()
        .with_context(|| format!("resolving SPB driver JAR {}", cfg.driver_jar.display()))?;

    let base = std::fs::read_to_string(cfg.scenario)
        .with_context(|| format!("reading SPB scenario file {}", cfg.scenario.display()))?;

    let update_url = cfg.endpoint_update.unwrap_or(cfg.endpoint);
    let merged = merge_properties(
        &base,
        &[
            ("endpointURL", cfg.endpoint),
            ("endpointUpdateURL", update_url),
            (
                "benchmarkRunPeriodSeconds",
                &cfg.duration_seconds.to_string(),
            ),
        ],
    );

    // The driver resolves the relative path keys in the scenario file
    // (`ontologiesPath=./data/...`, `definitionsPath=./...`, the
    // dictionary one level up from `referenceDatasetsPath`, etc.) against
    // its *working directory*. The conventional layout (the Ant build's
    // self-contained `dist/`) puts those paths relative to the scenario
    // file, so we run the driver with CWD = the scenario's directory and
    // co-locate the merged temp file there.
    let scenario_dir = cfg
        .scenario
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut merged_file = tempfile::Builder::new()
        .prefix("spb-scenario-")
        .suffix(".properties")
        .tempfile_in(scenario_dir)
        .context("creating merged SPB scenario temp file")?;
    use std::io::Write as _;
    merged_file
        .write_all(merged.as_bytes())
        .context("writing merged SPB scenario")?;
    merged_file
        .flush()
        .context("flushing merged SPB scenario")?;

    let output = Command::new("java")
        .arg("-jar")
        .arg(&driver_jar)
        .arg(merged_file.path())
        .current_dir(scenario_dir)
        .output()
        .with_context(|| "spawning `java` for the SPB driver (is a JRE installed and on PATH?)")?;
    if !output.status.success() {
        return Err(anyhow!(
            "SPB driver exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_report(&stdout)
}

/// Merge `overrides` into a Java `.properties` text. An existing
/// `key=...` line (ignoring leading whitespace) is replaced in place;
/// keys not already present are appended. Comments (`#`/`!`) and blank
/// lines are preserved.
fn merge_properties(base: &str, overrides: &[(&str, &str)]) -> String {
    let mut out_lines: Vec<String> = Vec::new();
    let mut applied = vec![false; overrides.len()];

    for line in base.lines() {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with('#') || trimmed.starts_with('!');
        let mut replaced = false;
        if !is_comment {
            if let Some(eq) = trimmed.find('=') {
                let key = trimmed[..eq].trim();
                if let Some(idx) = overrides.iter().position(|(k, _)| *k == key) {
                    out_lines.push(format!("{}={}", overrides[idx].0, overrides[idx].1));
                    applied[idx] = true;
                    replaced = true;
                }
            }
        }
        if !replaced {
            out_lines.push(line.to_string());
        }
    }

    for (idx, (k, v)) in overrides.iter().enumerate() {
        if !applied[idx] {
            out_lines.push(format!("{k}={v}"));
        }
    }

    let mut out = out_lines.join("\n");
    out.push('\n');
    out
}

/// Scrape the headline averages from the SPB reporter's text output.
/// The reporter prints a cumulative block ~once per second; we take the
/// **last** occurrence of each metric, which is the final cumulative
/// average for the run.
fn parse_report(stdout: &str) -> Result<SpbResult> {
    let mut editorial: Option<f64> = None;
    let mut aggregation: Option<f64> = None;
    let mut seconds: Option<f64> = None;

    for line in stdout.lines() {
        let t = line.trim();
        if let Some(v) = suffix_value(t, "average operations per second") {
            editorial = Some(v);
        } else if let Some(v) = suffix_value(t, "average queries per second") {
            aggregation = Some(v);
        } else if let Some(rest) = t.strip_prefix("Seconds :") {
            if let Ok(v) = rest.trim().parse::<f64>() {
                seconds = Some(v);
            }
        }
    }

    match (editorial, aggregation, seconds) {
        (
            Some(editorial_ops_per_sec),
            Some(aggregation_queries_per_sec),
            Some(run_duration_seconds),
        ) => Ok(SpbResult {
            editorial_ops_per_sec,
            aggregation_queries_per_sec,
            run_duration_seconds,
        }),
        _ => Err(anyhow!(
            "could not parse SPB headline metrics from driver output \
             (editorial={editorial:?}, aggregation={aggregation:?}, seconds={seconds:?}); \
             check that the scenario had runBenchmark=true and the run completed"
        )),
    }
}

/// If `line` ends with `label`, parse the whitespace-separated token
/// immediately before it as an `f64`. The SPB reporter formats these as
/// `\t\t%.4f average operations per second`.
fn suffix_value(line: &str, label: &str) -> Option<f64> {
    let head = line.strip_suffix(label)?;
    head.split_whitespace().next_back()?.parse::<f64>().ok()
}

pub fn record(db: &Db, run_id: &str, reasoner_name: &str, r: &SpbResult) -> Result<()> {
    // Metric keys (`editorial-qps`, `aggregation-qps`, `duration-s`) are a
    // stable reporting contract — `harness report --metric editorial-qps`
    // in nightly.yml and the README examples query them by name, so keep
    // them even though the Rust field for editorial is the more accurate
    // "ops per sec". (The old `update-qps` metric is gone: SPB folds
    // updates into editorial operations and reports no standalone rate.)
    db.record_metric(
        run_id,
        "ldbc-spb-256",
        Some(reasoner_name),
        "editorial-qps",
        r.editorial_ops_per_sec,
        "ops",
    )?;
    db.record_metric(
        run_id,
        "ldbc-spb-256",
        Some(reasoner_name),
        "aggregation-qps",
        r.aggregation_queries_per_sec,
        "qps",
    )?;
    db.record_metric(
        run_id,
        "ldbc-spb-256",
        Some(reasoner_name),
        "duration-s",
        r.run_duration_seconds,
        "s",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_existing_keys_in_place() {
        let base = "\
aggregationAgents=8
endpointURL=http://old/sparql
endpointUpdateURL=http://old/update
benchmarkRunPeriodSeconds=30
";
        let merged = merge_properties(
            base,
            &[
                ("endpointURL", "http://new/sparql"),
                ("endpointUpdateURL", "http://new/update"),
                ("benchmarkRunPeriodSeconds", "600"),
            ],
        );
        assert!(merged.contains("endpointURL=http://new/sparql"));
        assert!(merged.contains("endpointUpdateURL=http://new/update"));
        assert!(merged.contains("benchmarkRunPeriodSeconds=600"));
        // Untouched key preserved, old values gone.
        assert!(merged.contains("aggregationAgents=8"));
        assert!(!merged.contains("http://old/"));
        assert!(!merged.contains("benchmarkRunPeriodSeconds=30"));
    }

    #[test]
    fn appends_missing_keys_and_keeps_comments() {
        let base = "# scenario\naggregationAgents=8\n";
        let merged = merge_properties(base, &[("endpointURL", "http://x/sparql")]);
        assert!(merged.starts_with("# scenario\n"));
        assert!(merged.contains("aggregationAgents=8"));
        assert!(merged.contains("endpointURL=http://x/sparql"));
    }

    #[test]
    fn parses_headline_metrics_taking_last_block() {
        // Two cumulative blocks; the last one is the final average.
        let out = "\
Seconds : 1
\t\t12.0000 average operations per second
\t\t3.0000 average queries per second

Seconds : 600
\t\t123.4567 average operations per second
\t\t5.6789 average queries per second
";
        let r = parse_report(out).unwrap();
        assert_eq!(r.editorial_ops_per_sec, 123.4567);
        assert_eq!(r.aggregation_queries_per_sec, 5.6789);
        assert_eq!(r.run_duration_seconds, 600.0);
    }

    #[test]
    fn missing_metrics_is_an_error() {
        let out = "Loading ontologies...\nSeconds : 5\n";
        assert!(parse_report(out).is_err());
    }
}
