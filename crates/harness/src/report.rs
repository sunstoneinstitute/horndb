//! Trend-report queries (SPEC-01 F8).
//!
//! For Stage 0 we surface only the data primitives needed to wire up
//! `harness report` and assert F8 in CI: time-series fetch, geometric
//! mean over the window, and a >20% regression flag against the 7-day
//! median.

use anyhow::Result;

use crate::db::Db;

#[derive(Debug, Clone)]
pub struct TrendPoint {
    pub run_id: String,
    pub timestamp: String,
    pub value: f64,
}

#[derive(Debug, Clone)]
pub struct TrendReport {
    pub suite: String,
    pub metric: String,
    pub points: Vec<TrendPoint>,
    pub regression_flag: bool,
}

pub fn trend(db: &Db, suite: &str, metric: &str) -> Result<TrendReport> {
    let rows = db.metric_series(suite, metric)?;
    let points: Vec<TrendPoint> = rows
        .into_iter()
        .map(|(run_id, timestamp, value)| TrendPoint {
            run_id,
            timestamp,
            value,
        })
        .collect();
    let regression_flag = detect_regression(&points);
    Ok(TrendReport {
        suite: suite.to_string(),
        metric: metric.to_string(),
        points,
        regression_flag,
    })
}

/// One sample in a per-engine series, chronological (oldest first).
#[derive(Debug, Clone)]
pub struct SeriesPoint {
    /// Short calendar label for the run, e.g. `2026-06-25`.
    pub when: String,
    /// Short commit, e.g. `305a21f`.
    pub commit: String,
    pub value: f64,
}

/// Fetch the suite/metric series grouped by `dataset` (engine label),
/// each group ordered oldest-first. Datasets are returned with `horndb`
/// first (the subject of the benchmark), then the rest alphabetically —
/// so the A/B reference (`graphdb-free`) renders after it.
pub fn series_by_dataset(
    db: &Db,
    suite: &str,
    metric: &str,
) -> Result<Vec<(String, Vec<SeriesPoint>)>> {
    let rows = db.metric_series_by_dataset(suite, metric)?;
    let mut groups: Vec<(String, Vec<SeriesPoint>)> = Vec::new();
    for (dataset, commit, timestamp, value) in rows {
        let point = SeriesPoint {
            when: short_date(&timestamp),
            commit: short_commit(&commit),
            value,
        };
        match groups.iter_mut().find(|(d, _)| *d == dataset) {
            Some((_, pts)) => pts.push(point),
            None => groups.push((dataset, vec![point])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| dataset_rank(a).cmp(&dataset_rank(b)).then(a.cmp(b)));
    Ok(groups)
}

/// `horndb` sorts first, everything else after.
fn dataset_rank(dataset: &str) -> u8 {
    if dataset == "horndb" {
        0
    } else {
        1
    }
}

/// `2026-06-25T10:30:45Z` → `2026-06-25`; non-RFC3339 input passes through.
fn short_date(timestamp: &str) -> String {
    timestamp.get(..10).unwrap_or(timestamp).to_string()
}

/// `2026-06-25` → `06-25`, for compact chart x-axis labels.
fn month_day(date: &str) -> String {
    date.get(5..).unwrap_or(date).to_string()
}

fn short_commit(commit: &str) -> String {
    commit.get(..7).unwrap_or(commit).to_string()
}

/// `145.900` → `145.9`, `0.230` → `0.23`, `0.000` → `0`.
fn trim_num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Render a GitHub-flavoured-markdown benchmark summary: an A/B snapshot
/// table over every engine, plus a Mermaid `xychart-beta` line of the
/// primary engine's trend. Pure over the grouped series so it can be unit
/// tested without a database. Designed to be appended to
/// `$GITHUB_STEP_SUMMARY`.
///
/// The chart plots a *single* series (the primary engine, `horndb` when
/// present). Mermaid's `xychart-beta` has no legend and only a linear
/// scale, so overlaying engines that differ by orders of magnitude (early
/// HornDB ~0.2 qps vs GraphDB ~150 qps) would flatten the subject to the
/// axis. The table carries the cross-engine comparison; the chart shows
/// the subject closing the gap over time.
pub fn render_markdown(suite: &str, metric: &str, groups: &[(String, Vec<SeriesPoint>)]) -> String {
    let mut out = format!("## Benchmark — `{suite}` / `{metric}`\n\n");

    let non_empty: Vec<&(String, Vec<SeriesPoint>)> =
        groups.iter().filter(|(_, p)| !p.is_empty()).collect();
    if non_empty.is_empty() {
        out.push_str("_No benchmark data recorded yet._\n");
        return out;
    }

    // --- A/B snapshot table (latest run per engine, with delta on prior) ---
    out.push_str("| Engine | Latest | Prev | Δ | Runs | When |\n");
    out.push_str("|---|--:|--:|--:|--:|---|\n");
    for (dataset, pts) in &non_empty {
        let last = pts.last().expect("non-empty");
        let prev = pts.len().checked_sub(2).map(|i| pts[i].value);
        let delta = match prev {
            Some(p) if p != 0.0 => format!("{:+.1}%", (last.value - p) / p * 100.0),
            _ => "—".to_string(),
        };
        let prev_s = prev.map(trim_num).unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} ({}) |\n",
            dataset,
            trim_num(last.value),
            prev_s,
            delta,
            pts.len(),
            last.when,
            last.commit,
        ));
    }
    out.push('\n');

    // Cross-engine ratio line, primary vs each reference engine.
    let primary = &non_empty[0];
    let primary_latest = primary.1.last().expect("non-empty").value;
    for (dataset, pts) in non_empty.iter().skip(1) {
        let other = pts.last().expect("non-empty").value;
        if other != 0.0 {
            out.push_str(&format!(
                "`{}` is **{}×** `{}` ({} vs {}).\n\n",
                primary.0,
                trim_num(primary_latest / other),
                dataset,
                trim_num(primary_latest),
                trim_num(other),
            ));
        }
    }

    // --- Mermaid trend chart of the primary engine (last 20 runs) ---
    const MAX_POINTS: usize = 20;
    let pts = &primary.1;
    let start = pts.len().saturating_sub(MAX_POINTS);
    let window = &pts[start..];
    let unit = metric.rsplit('-').next().unwrap_or(metric);
    let top = window
        .iter()
        .map(|p| p.value)
        .fold(0.0_f64, f64::max)
        .max(f64::MIN_POSITIVE)
        * 1.15;
    let labels: Vec<String> = window
        .iter()
        .map(|p| format!("\"{}\"", month_day(&p.when)))
        .collect();
    let values: Vec<String> = window.iter().map(|p| trim_num(p.value)).collect();

    out.push_str("```mermaid\nxychart-beta\n");
    out.push_str(&format!(
        "    title \"{} {} — last {} run(s)\"\n",
        primary.0,
        metric,
        window.len()
    ));
    out.push_str(&format!("    x-axis [{}]\n", labels.join(", ")));
    out.push_str(&format!(
        "    y-axis \"{}\" 0 --> {}\n",
        unit,
        trim_num(top)
    ));
    out.push_str(&format!("    line [{}]\n", values.join(", ")));
    out.push_str("```\n");
    out
}

fn detect_regression(points: &[TrendPoint]) -> bool {
    // Newest point is points[0]. "7-day median" approximated as the
    // median of the next 7 points (Stage 0 — we have no real time
    // arithmetic yet, just ordinal index; revisit when F8 grows).
    if points.len() < 8 {
        return false;
    }
    let mut window: Vec<f64> = points[1..8].iter().map(|p| p.value).collect();
    window.sort_by(f64::total_cmp);
    let median = window[3];
    let latest = points[0].value;
    // Latency metric semantics: higher is worse. A regression is
    // latest > 1.20 * median.
    latest > median * 1.20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_regression_above_20_percent() {
        let mut points = vec![TrendPoint {
            run_id: "latest".into(),
            timestamp: "z".into(),
            value: 130.0,
        }];
        for i in 0..7 {
            points.push(TrendPoint {
                run_id: format!("p{i}"),
                timestamp: format!("y{i}"),
                value: 100.0,
            });
        }
        assert!(detect_regression(&points));
    }

    #[test]
    fn does_not_flag_within_20_percent() {
        let mut points = vec![TrendPoint {
            run_id: "latest".into(),
            timestamp: "z".into(),
            value: 115.0,
        }];
        for i in 0..7 {
            points.push(TrendPoint {
                run_id: format!("p{i}"),
                timestamp: format!("y{i}"),
                value: 100.0,
            });
        }
        assert!(!detect_regression(&points));
    }

    fn pt(when: &str, commit: &str, value: f64) -> SeriesPoint {
        SeriesPoint {
            when: when.into(),
            commit: commit.into(),
            value,
        }
    }

    #[test]
    fn markdown_renders_table_and_mermaid_chart() {
        let groups = vec![
            (
                "horndb".to_string(),
                vec![
                    pt("2026-06-24", "aaaaaaa", 0.21),
                    pt("2026-06-25", "bbbbbbb", 0.23),
                ],
            ),
            (
                "graphdb-free".to_string(),
                vec![pt("2026-06-25", "bbbbbbb", 145.9)],
            ),
        ];
        let md = render_markdown("ldbc-spb-256", "aggregation-qps", &groups);

        // Table: both engines, trimmed values, a computed delta, run count.
        assert!(md.contains("| Engine | Latest | Prev | Δ | Runs | When |"));
        assert!(md.contains("| horndb | 0.23 | 0.21 | +9.5% | 2 | 2026-06-25 (bbbbbbb) |"));
        assert!(md.contains("| graphdb-free | 145.9 | — | — | 1 | 2026-06-25 (bbbbbbb) |"));

        // Cross-engine ratio line (primary vs reference).
        assert!(md.contains("`horndb` is **0.002×** `graphdb-free`"));

        // Mermaid chart: primary series only, compact month-day labels.
        assert!(md.contains("```mermaid"));
        assert!(md.contains("xychart-beta"));
        assert!(md.contains("x-axis [\"06-24\", \"06-25\"]"));
        assert!(md.contains("y-axis \"qps\""));
        assert!(md.contains("line [0.21, 0.23]"));
    }

    #[test]
    fn markdown_handles_empty_series() {
        let md = render_markdown("ldbc-spb-256", "aggregation-qps", &[]);
        assert!(md.contains("_No benchmark data recorded yet._"));
        assert!(!md.contains("```mermaid"));
    }

    #[test]
    fn trim_num_drops_trailing_zeros() {
        assert_eq!(trim_num(145.9), "145.9");
        assert_eq!(trim_num(0.230), "0.23");
        assert_eq!(trim_num(0.0), "0");
        assert_eq!(trim_num(2.0), "2");
    }

    #[test]
    fn insufficient_history_does_not_flag() {
        let points = vec![TrendPoint {
            run_id: "x".into(),
            timestamp: "t".into(),
            value: 9999.0,
        }];
        assert!(!detect_regression(&points));
    }
}
