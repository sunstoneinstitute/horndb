//! Outcome of running a single test case, and the aggregate `Report`
//! produced by a runner pass.

use serde::{Deserialize, Serialize};

/// Three-valued result that mirrors what SPEC-01 F1 calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// Per-test outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub test_id: String,
    pub suite: String,
    pub status: Status,
    /// Required when `status == Skipped` or `status == Failed`.
    pub reason: Option<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Aggregate over one runner pass.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, outcome: Outcome) {
        self.outcomes.push(outcome);
    }

    pub fn count(&self, status: Status) -> usize {
        self.outcomes.iter().filter(|o| o.status == status).count()
    }

    pub fn passed(&self) -> usize { self.count(Status::Passed) }
    pub fn failed(&self) -> usize { self.count(Status::Failed) }
    pub fn skipped(&self) -> usize { self.count(Status::Skipped) }

    /// True if any test failed. Skips do not fail the report.
    pub fn has_failures(&self) -> bool {
        self.failed() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_has_no_failures() {
        let r = Report::new();
        assert_eq!(r.passed(), 0);
        assert_eq!(r.failed(), 0);
        assert_eq!(r.skipped(), 0);
        assert!(!r.has_failures());
    }

    #[test]
    fn report_counts_by_status() {
        let mut r = Report::new();
        r.push(Outcome { test_id: "a".into(), suite: "owl2".into(), status: Status::Passed, reason: None, duration_ms: 1 });
        r.push(Outcome { test_id: "b".into(), suite: "owl2".into(), status: Status::Failed, reason: Some("nope".into()), duration_ms: 1 });
        r.push(Outcome { test_id: "c".into(), suite: "owl2".into(), status: Status::Skipped, reason: Some("waived".into()), duration_ms: 0 });
        assert_eq!(r.passed(), 1);
        assert_eq!(r.failed(), 1);
        assert_eq!(r.skipped(), 1);
        assert!(r.has_failures());
    }

    #[test]
    fn skips_do_not_count_as_failures() {
        let mut r = Report::new();
        r.push(Outcome { test_id: "c".into(), suite: "owl2".into(), status: Status::Skipped, reason: Some("waived".into()), duration_ms: 0 });
        assert!(!r.has_failures());
    }
}
