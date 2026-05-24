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

fn detect_regression(points: &[TrendPoint]) -> bool {
    // Newest point is points[0]. "7-day median" approximated as the
    // median of the next 7 points (Stage 0 — we have no real time
    // arithmetic yet, just ordinal index; revisit when F8 grows).
    if points.len() < 8 {
        return false;
    }
    let mut window: Vec<f64> = points[1..8].iter().map(|p| p.value).collect();
    window.sort_by(|a, b| a.partial_cmp(b).unwrap());
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
