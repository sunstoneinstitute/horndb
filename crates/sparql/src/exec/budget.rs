//! The current query's memory budget (`max_query_memory`).
//!
//! **Thread-local, for the same reason [`crate::exec::cancel`] is.** The
//! server runs one query per blocking-pool thread and the operator tree is
//! `!Send`, so a thread-local *is* the per-query scope. Charging through a
//! parameter would mean threading it through every operator constructor and
//! every `Executor` method.
//!
//! # What this counts, and what it does not
//!
//! It counts the **executor's own row buffers**: the row sets that blocking
//! operators (GROUP BY, ORDER BY, UNION, the hash joins' build side,
//! property-path closure) accumulate whole before they can emit anything.
//! Those are the allocations with no upper bound but the data — a streaming
//! operator holds one chunk, a blocking one holds the entire input.
//!
//! It does **not** count the store, the dictionary, WCOJ iterator state, or
//! the response serialization buffer. A query is therefore always using
//! somewhat more process memory than its charge says. The budget is a bound
//! on the executor's growth, not an accounting of the process — see
//! `docs/specs/SPEC-31-query-memory-accounting.md` for what each phase adds.

use crate::error::{Result, SparqlError};
use std::cell::Cell;

thread_local! {
    /// Bytes charged by this thread's current query.
    static USED: Cell<u64> = const { Cell::new(0) };
    /// The query's ceiling, or `None` for unbounded.
    static LIMIT: Cell<Option<u64>> = const { Cell::new(None) };
    /// High-water mark, for the metric.
    static PEAK: Cell<u64> = const { Cell::new(0) };
}

/// Install `limit` as this thread's query budget until the guard drops.
///
/// The guard resets rather than restores: query scopes never nest, and
/// blocking-pool threads are reused, so a finished query's charge left
/// installed would bill the *next* query on this thread — the same
/// thread-reuse hazard [`crate::exec::cancel::scope`] and `phases::reset`
/// handle.
pub fn scope(limit: Option<u64>) -> Scope {
    USED.with(|u| u.set(0));
    PEAK.with(|p| p.set(0));
    LIMIT.with(|l| l.set(limit));
    Scope
}

/// Guard returned by [`scope`].
#[must_use = "the scope ends when this guard drops"]
pub struct Scope;

impl Drop for Scope {
    fn drop(&mut self) {
        // Recorded here rather than at a call site so it fires exactly once
        // per query on every path — clean finish, error, over-budget abort,
        // client disconnect. Zero-peak queries (anything with no blocking
        // operator) are observed too: "most queries accumulate nothing" is
        // itself the reading an operator wants from this histogram.
        horndb_metrics::metrics()
            .sparql
            .query_memory_peak_bytes
            .observe(PEAK.with(Cell::get) as f64);
        USED.with(|u| u.set(0));
        LIMIT.with(|l| l.set(None));
        PEAK.with(|p| p.set(0));
    }
}

/// Bytes currently charged on this thread.
pub fn used() -> u64 {
    USED.with(Cell::get)
}

/// The high-water mark since the scope opened. Read once per query, after
/// execution, to record `horndb_sparql_query_memory_peak_bytes`.
pub fn peak() -> u64 {
    PEAK.with(Cell::get)
}

/// This thread's ceiling, or `None` when unbounded.
pub fn limit() -> Option<u64> {
    LIMIT.with(Cell::get)
}

/// A charge against the budget, released when it drops.
///
/// An operator holds one for as long as it holds the rows it charged for, so
/// the budget tracks what is *live*, not what a query has cumulatively
/// touched. Dropping the rows without dropping the reservation would leak
/// budget for the rest of the query; the two are kept in the same struct
/// field for that reason.
#[derive(Debug, Default)]
pub struct Reservation {
    bytes: u64,
}

impl Reservation {
    /// An empty reservation, charging nothing.
    pub fn new() -> Self {
        Self { bytes: 0 }
    }

    /// Bytes this reservation is currently holding.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Charge `extra` more bytes, or fail with [`SparqlError::QueryMemoryLimit`]
    /// if that would cross the ceiling.
    ///
    /// The charge is rejected as a whole: on `Err` nothing is added, so a
    /// failed grow leaves the budget exactly as it was and the error can
    /// propagate without unwinding bookkeeping.
    pub fn grow(&mut self, extra: u64) -> Result<()> {
        let used = USED.with(Cell::get);
        let next = used.saturating_add(extra);
        if let Some(limit) = LIMIT.with(Cell::get) {
            if next > limit {
                horndb_metrics::metrics().sparql.queries_over_budget.inc();
                return Err(SparqlError::QueryMemoryLimit {
                    limit,
                    requested: next,
                });
            }
        }
        USED.with(|u| u.set(next));
        PEAK.with(|p| p.set(p.get().max(next)));
        self.bytes += extra;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        USED.with(|u| u.set(u.get().saturating_sub(self.bytes)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_outside_a_scope() {
        assert_eq!(limit(), None);
        let mut r = Reservation::new();
        r.grow(u64::MAX / 2).unwrap();
    }

    #[test]
    fn grow_fails_past_the_ceiling_and_charges_nothing() {
        let _g = scope(Some(1000));
        let mut r = Reservation::new();
        r.grow(600).unwrap();
        assert_eq!(used(), 600);
        let err = r.grow(500).unwrap_err();
        assert!(matches!(err, SparqlError::QueryMemoryLimit { .. }));
        assert_eq!(used(), 600, "a rejected grow charges nothing");
        assert_eq!(r.bytes(), 600);
    }

    #[test]
    fn dropping_a_reservation_releases_its_charge() {
        let _g = scope(Some(1000));
        {
            let mut r = Reservation::new();
            r.grow(800).unwrap();
            assert_eq!(used(), 800);
        }
        assert_eq!(used(), 0);
        // …and the freed budget is usable again.
        let mut r = Reservation::new();
        r.grow(900).unwrap();
    }

    #[test]
    fn peak_survives_a_release() {
        let _g = scope(Some(1000));
        {
            let mut r = Reservation::new();
            r.grow(900).unwrap();
        }
        assert_eq!(used(), 0);
        assert_eq!(peak(), 900, "the high-water mark is not undone by a drop");
    }

    #[test]
    fn a_reused_thread_does_not_inherit_the_previous_query() {
        {
            let _g = scope(Some(1000));
            let mut r = Reservation::new();
            r.grow(900).unwrap();
            std::mem::forget(r); // query aborted mid-flight, charge still live
        }
        assert_eq!(used(), 0, "the scope guard clears the charge");
        assert_eq!(limit(), None);
    }
}
