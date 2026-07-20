//! Per-tuple MVCC visibility primitives (SPEC-25 S1).
//!
//! Every stored tuple carries a `[begin, end)` lifetime in tier commit-version
//! terms (ADR-0018: the commit version is the engine's logical clock). A tuple
//! is visible at version `v` iff `begin <= v < end`. `end == UNSET_END` means
//! the tuple is still live (never retracted).

/// A tier commit version. Monotonic, bumped once per committed batch. `0` is
/// the empty store; the first commit is version `1`.
pub type CommitVersion = u64;

/// Sentinel `end` stamp for a live (never-retracted) tuple. No real commit
/// version reaches `u64::MAX`, so `v < UNSET_END` is always true for a live row.
pub const UNSET_END: CommitVersion = u64::MAX;

/// The highest queryable version — "latest committed". NOT `UNSET_END`:
/// `visible()`'s bound is strict (`at < end`), so a live row (`end == UNSET_END`)
/// would be invisible at `at == UNSET_END`. Query at `LATEST` to see all live rows.
pub const LATEST: CommitVersion = u64::MAX - 1;

/// True if a tuple stamped `[begin, end)` is visible at version `at`.
#[inline]
pub fn visible(begin: CommitVersion, end: CommitVersion, at: CommitVersion) -> bool {
    begin <= at && at < end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_tuple_visible_from_its_begin_onward() {
        // Inserted at v=5, never retracted.
        assert!(!visible(5, UNSET_END, 4), "not yet inserted");
        assert!(
            visible(5, UNSET_END, 5),
            "visible at its own insert version"
        );
        assert!(visible(5, UNSET_END, 999), "still visible far later");
    }

    #[test]
    fn retraction_takes_effect_at_its_own_version() {
        // Inserted at v=5, retracted at v=8.
        assert!(visible(5, 8, 7), "visible just before retraction");
        assert!(!visible(5, 8, 8), "hidden at the retraction version");
        assert!(!visible(5, 8, 9), "still hidden after");
    }
}
