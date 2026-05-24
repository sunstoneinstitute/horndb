//! SQLite result database (SPEC-01 F7).
//!
//! Schema (Stage 1):
//!
//! ```sql
//! CREATE TABLE runs (
//!     run_id        TEXT PRIMARY KEY,
//!     commit_sha    TEXT NOT NULL,
//!     hardware_id   TEXT NOT NULL,
//!     reasoner_name TEXT NOT NULL,
//!     started_at    TEXT NOT NULL  -- RFC3339
//! );
//! CREATE TABLE outcomes (
//!     run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
//!     suite       TEXT NOT NULL,
//!     test_id     TEXT NOT NULL,
//!     status      TEXT NOT NULL CHECK(status IN ('passed','failed','skipped')),
//!     reason      TEXT,
//!     duration_ms INTEGER NOT NULL
//! );
//! CREATE TABLE metrics (
//!     run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
//!     suite       TEXT NOT NULL,
//!     dataset     TEXT,
//!     metric_name TEXT NOT NULL,
//!     metric_value REAL NOT NULL,
//!     units       TEXT NOT NULL,
//!     timestamp   TEXT NOT NULL
//! );
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::outcome::{Outcome, Status};

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite db {}", path.display()))?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    pub(crate) fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                run_id        TEXT PRIMARY KEY,
                commit_sha    TEXT NOT NULL,
                hardware_id   TEXT NOT NULL,
                reasoner_name TEXT NOT NULL,
                started_at    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS outcomes (
                run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                suite       TEXT NOT NULL,
                test_id     TEXT NOT NULL,
                status      TEXT NOT NULL CHECK(status IN ('passed','failed','skipped')),
                reason      TEXT,
                duration_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS metrics (
                run_id       TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                suite        TEXT NOT NULL,
                dataset      TEXT,
                metric_name  TEXT NOT NULL,
                metric_value REAL NOT NULL,
                units        TEXT NOT NULL,
                timestamp    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_outcomes_run ON outcomes(run_id);
            CREATE INDEX IF NOT EXISTS idx_metrics_run ON metrics(run_id);
            "#,
        )?;
        Ok(())
    }

    /// Begin a new run; returns the synthesised `run_id`.
    pub fn start_run(
        &self,
        commit_sha: &str,
        hardware_id: &str,
        reasoner_name: &str,
    ) -> Result<String> {
        let run_id = new_run_id(commit_sha, reasoner_name);
        let now =
            OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
        self.conn.execute(
            "INSERT INTO runs (run_id, commit_sha, hardware_id, reasoner_name, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, commit_sha, hardware_id, reasoner_name, now],
        )?;
        Ok(run_id)
    }

    pub fn record_outcome(&self, run_id: &str, o: &Outcome) -> Result<()> {
        let status = match o.status {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
        };
        self.conn.execute(
            "INSERT INTO outcomes (run_id, suite, test_id, status, reason, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, o.suite, o.test_id, status, o.reason, o.duration_ms as i64],
        )?;
        Ok(())
    }

    /// Number of outcomes recorded against a given run.
    pub fn outcomes_for(&self, run_id: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM outcomes WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    pub fn record_metric(
        &self,
        run_id: &str,
        suite: &str,
        dataset: Option<&str>,
        metric_name: &str,
        metric_value: f64,
        units: &str,
    ) -> Result<()> {
        let now =
            OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
        self.conn.execute(
            "INSERT INTO metrics (run_id, suite, dataset, metric_name, metric_value, units, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![run_id, suite, dataset, metric_name, metric_value, units, now],
        )?;
        Ok(())
    }

    /// Returns `(run_id, timestamp_rfc3339, metric_value)` rows for the
    /// given suite/metric, newest first.
    pub fn metric_series(
        &self,
        suite: &str,
        metric_name: &str,
    ) -> Result<Vec<(String, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timestamp, metric_value FROM metrics
             WHERE suite = ?1 AND metric_name = ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt
            .query_map(params![suite, metric_name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn new_run_id(commit_sha: &str, reasoner_name: &str) -> String {
    let mut h = Sha256::new();
    h.update(commit_sha.as_bytes());
    h.update(b":");
    h.update(reasoner_name.as_bytes());
    h.update(b":");
    h.update(
        OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .to_string()
            .as_bytes(),
    );
    hex::encode(&h.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, status: Status) -> Outcome {
        Outcome {
            test_id: id.into(),
            suite: "owl2".into(),
            status,
            reason: None,
            duration_ms: 1,
        }
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run("deadbeef", "fingerprint-1", "stub").unwrap();
        assert_eq!(db.outcomes_for(&run).unwrap(), 0);
    }

    #[test]
    fn records_and_counts_outcomes() {
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run("deadbeef", "fingerprint-1", "stub").unwrap();
        db.record_outcome(&run, &outcome("a", Status::Passed))
            .unwrap();
        db.record_outcome(&run, &outcome("b", Status::Failed))
            .unwrap();
        db.record_outcome(&run, &outcome("c", Status::Skipped))
            .unwrap();
        assert_eq!(db.outcomes_for(&run).unwrap(), 3);
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
    }
}
