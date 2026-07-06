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
//! The driver's reporter (`TestDriverReporter.java`) prints a one-off
//! header followed by a cumulative summary block roughly once per
//! second; the final block before shutdown carries the run's headline
//! averages. We scrape the **last** occurrence of every number the
//! reporter emits, so the whole driver report lands in the harness trend
//! DB — not just the three headline rates:
//!   - Headline (**required** — their absence means the run did not
//!     complete): `N.NNNN average operations per second` → editorial
//!     throughput (CW inserts + updates + deletes combined; SPB reports
//!     no standalone "update QPS"); `N.NNNN average queries per second`
//!     → aggregation throughput; `Seconds : N` → measured run duration.
//!   - Dataset info (header): Creative Works / Reference Entities / Geo
//!     Locations counts (printed with `,` thousands separators).
//!   - `(completed query mixes : N)` or `(completed query runs : N)`.
//!   - Editorial: total operations, per-op (insert/update/delete) counts,
//!     verbose-only per-op avg/min/max ms and per-op error counts.
//!   - Aggregation: per-query-type `Q1..Qn` count, verbose-only
//!     avg/min/max ms + per-query error count; total retrieval queries
//!     and (verbose) total errors.
//!
//! Every non-headline section is **optional**: a non-verbose run, or a
//! scenario that omits a section, simply yields fewer metrics rather than
//! an error. We never fabricate zeros for fields the driver did not print.
//!
//! Stage-1 scope: SPB-256 (SF=0.256, ~256M triples). SF3/SF5 are
//! Stage-2.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;

use crate::db::Db;

/// `avg : N ms, min : N ms, max : N ms` execution-time triple the driver
/// prints (verbose mode) for a query type or editorial operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub avg_ms: u64,
    pub min_ms: u64,
    pub max_ms: u64,
}

/// One aggregation query type (`Q1..Qn`). `count` is always present;
/// `timing` and `errors` are populated only in the driver's verbose mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryStat {
    pub id: u32,
    pub count: u64,
    pub timing: Option<Timing>,
    pub errors: Option<u64>,
}

/// One editorial operation kind (insert / update / delete). `count` comes
/// from the always-present "operations" totals line; `timing` and `errors`
/// are verbose-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpStat {
    pub count: u64,
    pub timing: Option<Timing>,
    pub errors: Option<u64>,
}

/// Editorial (write-side) breakdown from the final cumulative block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorialStats {
    /// `%d operations (...)` — total CW inserts + updates + deletes.
    pub total_ops: u64,
    pub inserts: OpStat,
    pub updates: OpStat,
    pub deletes: OpStat,
}

/// Dataset sizes from the reporter's one-off header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetInfo {
    pub creative_works: u64,
    pub reference_entities: u64,
    pub geo_locations: u64,
}

/// The driver reports progress either as completed query-*mix* runs or as
/// completed individual query runs, depending on the scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completed {
    Mixes(u64),
    Runs(u64),
}

/// The full SPB driver report, scraped from the final cumulative block
/// (plus the one-off header). The three headline rates are always present;
/// every other field is optional (a non-verbose run, or one with no
/// editorial agents, yields fewer of them). SPB reports no standalone
/// update rate, so there is no `update_qps`.
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
    /// Per aggregation query type (`Q1..Qn`), final-block values.
    pub per_query: Vec<QueryStat>,
    /// Editorial breakdown (present whenever the "operations" line printed).
    pub editorial: Option<EditorialStats>,
    /// `%d total retrieval queries` — aggregation operations completed.
    pub aggregation_total_queries: Option<u64>,
    /// Aggregation query errors (verbose only).
    pub aggregation_errors: Option<u64>,
    /// Dataset sizes from the header.
    pub dataset: Option<DatasetInfo>,
    /// Completed query mixes / runs from the final block.
    pub completed: Option<Completed>,
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

/// Scrape the full SPB reporter text output. The reporter prints a one-off
/// header then a cumulative block ~once per second; for every metric we
/// keep the **last** occurrence, which is the final cumulative value for
/// the run. The three headline rates are required (their absence means the
/// run did not complete); every other section is optional.
fn parse_report(stdout: &str) -> Result<SpbResult> {
    // Headline (required).
    let mut editorial_qps: Option<f64> = None;
    let mut aggregation_qps: Option<f64> = None;
    let mut seconds: Option<f64> = None;

    // Optional sections, all last-occurrence-wins.
    let (mut ds_cw, mut ds_ref, mut ds_geo): (Option<u64>, Option<u64>, Option<u64>) =
        (None, None, None);
    let mut completed: Option<Completed> = None;
    let mut agg_total: Option<u64> = None;
    let mut agg_errors: Option<u64> = None;
    let mut ed_total_ops: Option<u64> = None;
    let mut ed_counts: Option<(u64, u64, u64)> = None; // (insert, update, delete)
    let mut ed_errors: Option<(u64, u64, u64)> = None;
    let mut ed_insert_t: Option<Timing> = None;
    let mut ed_update_t: Option<Timing> = None;
    let mut ed_delete_t: Option<Timing> = None;
    let mut per_query: BTreeMap<u32, QueryStat> = BTreeMap::new();

    // Variable `%-5d`/`%-7d` padding makes fixed-offset parsing brittle, so
    // match the reporter's lines with whitespace-tolerant regexes.
    let re_dataset =
        Regex::new(r"(Creative Works|Reference Entities|Geo Locations)\s*:\s*([\d,]+)").unwrap();
    let re_completed = Regex::new(r"completed query (mixes|runs) : (\d+)").unwrap();
    let re_query = Regex::new(
        r"^(\d+)\s+Q(\d+)\s+queries(?:\s+\(avg : (\d+)\s+ms, min : (\d+)\s+ms, max : (\d+)\s+ms, (\d+) errors\))?",
    )
    .unwrap();
    let re_editop = Regex::new(
        r"^(\d+)\s+(inserts|updates|deletes)\s+\(avg : (\d+)\s+ms, min : (\d+)\s+ms, max : (\d+)\s+ms\)",
    )
    .unwrap();
    let re_ops_verbose = Regex::new(
        r"^(\d+) operations \((\d+) CW Inserts \((\d+) errors\), (\d+) CW Updates \((\d+) errors\), (\d+) CW Deletions \((\d+) errors\)\)",
    )
    .unwrap();
    let re_ops_plain =
        Regex::new(r"^(\d+) operations \((\d+) CW Inserts, (\d+) CW Updates, (\d+) CW Deletions\)")
            .unwrap();
    let re_agg_verbose = Regex::new(r"^(\d+) total retrieval queries \((\d+) errors\)").unwrap();
    let re_agg_plain = Regex::new(r"^(\d+) total retrieval queries\s*$").unwrap();

    for line in stdout.lines() {
        let t = line.trim();

        // Headline averages + run duration (same last-occurrence behaviour
        // the shim has always had).
        if let Some(v) = suffix_value(t, "average operations per second") {
            editorial_qps = Some(v);
            continue;
        }
        if let Some(v) = suffix_value(t, "average queries per second") {
            aggregation_qps = Some(v);
            continue;
        }
        if let Some(rest) = t.strip_prefix("Seconds :") {
            if let Ok(v) = rest.trim().parse::<f64>() {
                seconds = Some(v);
            }
            continue;
        }

        // Dataset info (header, `%,d` thousands separators).
        for c in re_dataset.captures_iter(t) {
            let n = parse_grouped_u64(&c[2]);
            match &c[1] {
                "Creative Works" => ds_cw = n,
                "Reference Entities" => ds_ref = n,
                "Geo Locations" => ds_geo = n,
                _ => {}
            }
        }

        if let Some(c) = re_completed.captures(t) {
            if let Ok(n) = c[2].parse::<u64>() {
                completed = Some(if &c[1] == "mixes" {
                    Completed::Mixes(n)
                } else {
                    Completed::Runs(n)
                });
            }
            continue;
        }

        if let Some(c) = re_query.captures(t) {
            let count: u64 = c[1].parse().unwrap_or_default();
            let id: u32 = c[2].parse().unwrap_or_default();
            let timing = match (c.get(3), c.get(4), c.get(5)) {
                (Some(a), Some(mn), Some(mx)) => Some(Timing {
                    avg_ms: a.as_str().parse().unwrap_or_default(),
                    min_ms: mn.as_str().parse().unwrap_or_default(),
                    max_ms: mx.as_str().parse().unwrap_or_default(),
                }),
                _ => None,
            };
            let errors = c.get(6).and_then(|m| m.as_str().parse::<u64>().ok());
            per_query.insert(
                id,
                QueryStat {
                    id,
                    count,
                    timing,
                    errors,
                },
            );
            continue;
        }

        if let Some(c) = re_editop.captures(t) {
            let timing = Timing {
                avg_ms: c[3].parse().unwrap_or_default(),
                min_ms: c[4].parse().unwrap_or_default(),
                max_ms: c[5].parse().unwrap_or_default(),
            };
            match &c[2] {
                "inserts" => ed_insert_t = Some(timing),
                "updates" => ed_update_t = Some(timing),
                "deletes" => ed_delete_t = Some(timing),
                _ => {}
            }
            continue;
        }

        if let Some(c) = re_ops_verbose.captures(t) {
            ed_total_ops = c[1].parse().ok();
            ed_counts = Some((
                c[2].parse().unwrap_or_default(),
                c[4].parse().unwrap_or_default(),
                c[6].parse().unwrap_or_default(),
            ));
            ed_errors = Some((
                c[3].parse().unwrap_or_default(),
                c[5].parse().unwrap_or_default(),
                c[7].parse().unwrap_or_default(),
            ));
            continue;
        }
        if let Some(c) = re_ops_plain.captures(t) {
            ed_total_ops = c[1].parse().ok();
            ed_counts = Some((
                c[2].parse().unwrap_or_default(),
                c[3].parse().unwrap_or_default(),
                c[4].parse().unwrap_or_default(),
            ));
            ed_errors = None;
            continue;
        }

        if let Some(c) = re_agg_verbose.captures(t) {
            agg_total = c[1].parse().ok();
            agg_errors = c[2].parse().ok();
            continue;
        }
        if let Some(c) = re_agg_plain.captures(t) {
            agg_total = c[1].parse().ok();
            agg_errors = None;
            continue;
        }
    }

    let dataset = match (ds_cw, ds_ref, ds_geo) {
        (Some(creative_works), Some(reference_entities), Some(geo_locations)) => {
            Some(DatasetInfo {
                creative_works,
                reference_entities,
                geo_locations,
            })
        }
        _ => None,
    };

    let editorial = match (ed_total_ops, ed_counts) {
        (Some(total_ops), Some((ic, uc, dc))) => {
            let (ie, ue, de) = match ed_errors {
                Some((ie, ue, de)) => (Some(ie), Some(ue), Some(de)),
                None => (None, None, None),
            };
            Some(EditorialStats {
                total_ops,
                inserts: OpStat {
                    count: ic,
                    timing: ed_insert_t,
                    errors: ie,
                },
                updates: OpStat {
                    count: uc,
                    timing: ed_update_t,
                    errors: ue,
                },
                deletes: OpStat {
                    count: dc,
                    timing: ed_delete_t,
                    errors: de,
                },
            })
        }
        _ => None,
    };

    match (editorial_qps, aggregation_qps, seconds) {
        (
            Some(editorial_ops_per_sec),
            Some(aggregation_queries_per_sec),
            Some(run_duration_seconds),
        ) => Ok(SpbResult {
            editorial_ops_per_sec,
            aggregation_queries_per_sec,
            run_duration_seconds,
            per_query: per_query.into_values().collect(),
            editorial,
            aggregation_total_queries: agg_total,
            aggregation_errors: agg_errors,
            dataset,
            completed,
        }),
        _ => Err(anyhow!(
            "could not parse SPB headline metrics from driver output \
             (editorial={editorial_qps:?}, aggregation={aggregation_qps:?}, seconds={seconds:?}); \
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

/// Parse a `%,d`-formatted integer (thousands separators) like `1,234,567`.
fn parse_grouped_u64(s: &str) -> Option<u64> {
    s.replace(',', "").parse::<u64>().ok()
}

pub fn record(db: &Db, run_id: &str, reasoner_name: &str, r: &SpbResult) -> Result<()> {
    let dataset = Some(reasoner_name);
    let put = |name: &str, value: f64, units: &str| -> Result<()> {
        db.record_metric(run_id, "ldbc-spb-256", dataset, name, value, units)
    };

    // Headline keys (`editorial-qps`, `aggregation-qps`, `duration-s`) are a
    // stable reporting contract — `harness report --metric editorial-qps`
    // in nightly.yml and the README examples query them by name, so keep
    // them verbatim even though the Rust field for editorial is the more
    // accurate "ops per sec". (The old `update-qps` metric is gone: SPB
    // folds updates into editorial operations and reports no standalone
    // rate.)
    put("editorial-qps", r.editorial_ops_per_sec, "ops")?;
    put("aggregation-qps", r.aggregation_queries_per_sec, "qps")?;
    put("duration-s", r.run_duration_seconds, "s")?;

    // Per aggregation query type (`Q1..Qn`). Timing/errors only when the
    // driver ran verbose; counts always.
    for q in &r.per_query {
        put(&format!("q{}-count", q.id), q.count as f64, "ops")?;
        if let Some(t) = &q.timing {
            put(&format!("q{}-avg-ms", q.id), t.avg_ms as f64, "ms")?;
            put(&format!("q{}-min-ms", q.id), t.min_ms as f64, "ms")?;
            put(&format!("q{}-max-ms", q.id), t.max_ms as f64, "ms")?;
        }
        if let Some(e) = q.errors {
            put(&format!("q{}-errors", q.id), e as f64, "count")?;
        }
    }

    // Aggregation totals.
    if let Some(n) = r.aggregation_total_queries {
        put("aggregation-total-queries", n as f64, "ops")?;
    }
    if let Some(n) = r.aggregation_errors {
        put("aggregation-errors", n as f64, "count")?;
    }

    // Editorial breakdown.
    if let Some(ed) = &r.editorial {
        put("editorial-total-ops", ed.total_ops as f64, "ops")?;
        for (prefix, op) in [
            ("insert", &ed.inserts),
            ("update", &ed.updates),
            ("delete", &ed.deletes),
        ] {
            put(&format!("editorial-{prefix}-count"), op.count as f64, "ops")?;
            if let Some(t) = &op.timing {
                put(&format!("editorial-{prefix}-avg-ms"), t.avg_ms as f64, "ms")?;
                put(&format!("editorial-{prefix}-min-ms"), t.min_ms as f64, "ms")?;
                put(&format!("editorial-{prefix}-max-ms"), t.max_ms as f64, "ms")?;
            }
            if let Some(e) = op.errors {
                put(&format!("editorial-{prefix}-errors"), e as f64, "count")?;
            }
        }
    }

    // Dataset sizes (header).
    if let Some(ds) = &r.dataset {
        put("cw-count", ds.creative_works as f64, "count")?;
        put("reference-entities", ds.reference_entities as f64, "count")?;
        put("geo-locations", ds.geo_locations as f64, "count")?;
    }

    // Completed query mixes / runs.
    match r.completed {
        Some(Completed::Mixes(n)) => put("completed-query-mixes", n as f64, "count")?,
        Some(Completed::Runs(n)) => put("completed-query-runs", n as f64, "count")?,
        None => {}
    }

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

    /// A header + two **verbose** cumulative blocks. Block 1 carries small
    /// numbers, block 2 the final cumulative values — the parser must report
    /// block 2 throughout. Spacing mirrors the driver's `%-5d` / `%-7d`
    /// padding (the parser tolerates variable whitespace).
    fn verbose_two_block_report() -> String {
        "\
LDBC Semantic Publishing Benchmark
Started: 2026-06-30 03:00:00
Dataset Info: \tCreative Works\t: 1,234,567
\tReference Entities\t: 12,345
\tGeo Locations\t\t: 6,789

Benchmark Results:

Seconds : 1
2026-06-30 03:00:01 (completed query mixes : 1)
\tEditorial:
\t\t2 agents

\t\t1     inserts (avg : 5       ms, min : 5       ms, max : 5       ms)
\t\t0     updates (avg : 0       ms, min : 0       ms, max : 0       ms)
\t\t0     deletes (avg : 0       ms, min : 0       ms, max : 0       ms)

\t\t1 operations (1 CW Inserts (0 errors), 0 CW Updates (0 errors), 0 CW Deletions (0 errors))
\t\t1.0000 average operations per second
\tAggregation:
\t\t4 agents

\t\t10    Q1   queries (avg : 9       ms, min : 2       ms, max : 30      ms, 0 errors)
\t\t8     Q2   queries (avg : 15      ms, min : 4       ms, max : 60      ms, 0 errors)

\t\t18 total retrieval queries (0 errors)
\t\t3.0000 average queries per second

Seconds : 600
2026-06-30 03:10:00 (completed query mixes : 42)
\tEditorial:
\t\t2 agents

\t\t120   inserts (avg : 7       ms, min : 2       ms, max : 41      ms)
\t\t30    updates (avg : 11      ms, min : 3       ms, max : 88      ms)
\t\t6     deletes (avg : 9       ms, min : 4       ms, max : 22      ms)

\t\t156 operations (120 CW Inserts (1 errors), 30 CW Updates (2 errors), 6 CW Deletions (0 errors))
\t\t0.2600 average operations per second
\tAggregation:
\t\t4 agents

\t\t1000  Q1   queries (avg : 12      ms, min : 3       ms, max : 55      ms, 1 errors)
\t\t900   Q2   queries (avg : 20      ms, min : 5       ms, max : 80      ms, 3 errors)

\t\t1900 total retrieval queries (4 errors)
\t\t5.6789 average queries per second
"
        .to_string()
    }

    #[test]
    fn parses_full_verbose_report_taking_last_block() {
        let r = parse_report(&verbose_two_block_report()).unwrap();

        // Headline (final block).
        assert_eq!(r.editorial_ops_per_sec, 0.2600);
        assert_eq!(r.aggregation_queries_per_sec, 5.6789);
        assert_eq!(r.run_duration_seconds, 600.0);

        // Dataset info from the header (thousands separators stripped).
        let ds = r.dataset.expect("dataset info");
        assert_eq!(ds.creative_works, 1_234_567);
        assert_eq!(ds.reference_entities, 12_345);
        assert_eq!(ds.geo_locations, 6_789);

        // Completed query mixes from the final block.
        assert_eq!(r.completed, Some(Completed::Mixes(42)));

        // Per-query: final-block values, with timing + errors.
        assert_eq!(r.per_query.len(), 2);
        let q1 = &r.per_query[0];
        assert_eq!(q1.id, 1);
        assert_eq!(q1.count, 1000);
        assert_eq!(
            q1.timing,
            Some(Timing {
                avg_ms: 12,
                min_ms: 3,
                max_ms: 55
            })
        );
        assert_eq!(q1.errors, Some(1));
        let q2 = &r.per_query[1];
        assert_eq!(q2.id, 2);
        assert_eq!(q2.count, 900);
        assert_eq!(q2.errors, Some(3));

        // Aggregation totals.
        assert_eq!(r.aggregation_total_queries, Some(1900));
        assert_eq!(r.aggregation_errors, Some(4));

        // Editorial breakdown (final block).
        let ed = r.editorial.expect("editorial stats");
        assert_eq!(ed.total_ops, 156);
        assert_eq!(ed.inserts.count, 120);
        assert_eq!(
            ed.inserts.timing,
            Some(Timing {
                avg_ms: 7,
                min_ms: 2,
                max_ms: 41
            })
        );
        assert_eq!(ed.inserts.errors, Some(1));
        assert_eq!(ed.updates.count, 30);
        assert_eq!(ed.updates.errors, Some(2));
        assert_eq!(ed.deletes.count, 6);
        assert_eq!(ed.deletes.errors, Some(0));
    }

    /// Non-verbose mode collapses the detail lines to bare counts. Timing and
    /// error fields must be `None` (we never fabricate zeros), counts present.
    fn non_verbose_report() -> String {
        "\
LDBC Semantic Publishing Benchmark
Dataset Info: \tCreative Works\t: 500
\tReference Entities\t: 50
\tGeo Locations\t\t: 5

Seconds : 600
2026-06-30 03:10:00 (completed query runs : 1900)
\tEditorial:
\t\t2 agents

\t\t156 operations (120 CW Inserts, 30 CW Updates, 6 CW Deletions)
\t\t0.2600 average operations per second
\tAggregation:
\t\t4 agents

\t\t1000  Q1   queries
\t\t900   Q2   queries

\t\t1900 total retrieval queries
\t\t5.6789 average queries per second
"
        .to_string()
    }

    #[test]
    fn parses_non_verbose_report_count_only() {
        let r = parse_report(&non_verbose_report()).unwrap();

        assert_eq!(r.completed, Some(Completed::Runs(1900)));

        assert_eq!(r.per_query.len(), 2);
        assert_eq!(r.per_query[0].count, 1000);
        assert_eq!(r.per_query[0].timing, None);
        assert_eq!(r.per_query[0].errors, None);

        let ed = r.editorial.expect("editorial stats");
        assert_eq!(ed.total_ops, 156);
        assert_eq!(ed.inserts.count, 120);
        assert_eq!(ed.inserts.timing, None);
        assert_eq!(ed.inserts.errors, None);
        assert_eq!(ed.deletes.count, 6);

        assert_eq!(r.aggregation_total_queries, Some(1900));
        assert_eq!(r.aggregation_errors, None);
    }

    #[test]
    fn record_writes_full_metric_set_to_db() {
        let db = Db::open_in_memory().unwrap();
        let run_id = db.start_run("deadbeef", "test-hw", "horndb").unwrap();
        let r = parse_report(&verbose_two_block_report()).unwrap();
        record(&db, &run_id, "horndb", &r).unwrap();

        // Helper: fetch the single value recorded for a metric under `horndb`.
        let value = |name: &str| -> Option<f64> {
            db.metric_series("ldbc-spb-256", name)
                .unwrap()
                .first()
                .map(|(_, _, v)| *v)
        };

        // Legacy headline names/values unchanged.
        assert_eq!(value("editorial-qps"), Some(0.2600));
        assert_eq!(value("aggregation-qps"), Some(5.6789));
        assert_eq!(value("duration-s"), Some(600.0));

        // Per-query.
        assert_eq!(value("q1-count"), Some(1000.0));
        assert_eq!(value("q1-avg-ms"), Some(12.0));
        assert_eq!(value("q1-min-ms"), Some(3.0));
        assert_eq!(value("q1-max-ms"), Some(55.0));
        assert_eq!(value("q1-errors"), Some(1.0));
        assert_eq!(value("q2-count"), Some(900.0));

        // Aggregation + editorial totals.
        assert_eq!(value("aggregation-total-queries"), Some(1900.0));
        assert_eq!(value("aggregation-errors"), Some(4.0));
        assert_eq!(value("editorial-total-ops"), Some(156.0));
        assert_eq!(value("editorial-insert-count"), Some(120.0));
        assert_eq!(value("editorial-insert-avg-ms"), Some(7.0));
        assert_eq!(value("editorial-update-errors"), Some(2.0));
        assert_eq!(value("editorial-delete-count"), Some(6.0));

        // Dataset info + completed.
        assert_eq!(value("cw-count"), Some(1_234_567.0));
        assert_eq!(value("reference-entities"), Some(12_345.0));
        assert_eq!(value("geo-locations"), Some(6_789.0));
        assert_eq!(value("completed-query-mixes"), Some(42.0));
    }
}
