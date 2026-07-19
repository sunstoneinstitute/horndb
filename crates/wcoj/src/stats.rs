//! Layered read-only statistics seam.
//!
//! A later cardinality estimator (SPEC-23 Phase 3) reads from these types to
//! bound query output sizes. This module defines the [`Stats`] trait, its data
//! types, and [`ZeroStats`] — the deliberately conservative fallback used when
//! no real statistics have been gathered yet.

use std::collections::HashMap;

use crate::ids::{Ordering, TermId};
use crate::pattern::TriplePattern;
use crate::source::vec_source::VecTripleSource;
use crate::source::TripleSource;

/// Which side of a triple a per-predicate statistic is keyed on. The predicate
/// is always bound in per-predicate stats, so only subject and object vary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Subject,
    Object,
}

/// Degree role — the same subject/object axis, named for degree lookups.
pub type Role = Position;

/// A cardinality estimate with an upper bound. `estimate` is the expected size;
/// `upper_bound` is a value the true size never exceeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Estimate {
    pub estimate: u64,
    pub upper_bound: u64,
}

/// One characteristic set: the exact predicate-set shared by a group of subjects.
pub struct CharacteristicSet {
    /// Sorted, distinct predicates — the set key.
    pub predicates: Vec<TermId>,
    /// Number of subjects whose predicate-set is exactly `predicates`.
    pub count: u64,
    /// Sorted by predicate: total objects for that predicate across the `count`
    /// subjects.
    pub occurrences: Vec<(TermId, u64)>,
}

/// Top-K frequent characteristic sets plus a residual bucket that folds the
/// rare-set tail into aggregate counts.
pub struct CharacteristicSetIndex {
    /// Top-K sets by `count`, descending.
    pub sets: Vec<CharacteristicSet>,
    /// Number of subjects in the folded tail.
    pub residual_subjects: u64,
    /// Predicate -> object occurrences within the tail.
    pub residual_pred_occ: Vec<(TermId, u64)>,
    /// Predicate -> indices into `sets` that contain it.
    pub by_predicate: std::collections::HashMap<TermId, Vec<usize>>,
}

impl CharacteristicSetIndex {
    /// An index with no sets and an empty residual bucket.
    pub fn empty() -> Self {
        Self {
            sets: Vec::new(),
            residual_subjects: 0,
            residual_pred_occ: Vec::new(),
            by_predicate: std::collections::HashMap::new(),
        }
    }
}

/// Tier-2 design-for stub. A degree summary (SafeBound / LpBound) is a later
/// phase; this placeholder marks the seam.
pub struct DegreeSummary;

/// Read-only statistics an estimator consumes. All methods are cheap lookups.
pub trait Stats: Send + Sync {
    /// Total number of triples in the graph.
    fn total_triples(&self) -> u64;
    /// Number of triples with predicate `p`.
    fn predicate_count(&self, p: TermId) -> u64;
    /// Number of distinct values on side `pos` for predicate `p`.
    fn ndv(&self, p: TermId, pos: Position) -> u64;
    /// The characteristic-set index.
    fn characteristic_sets(&self) -> &CharacteristicSetIndex;
    /// Maximum degree of any node on side `role` for predicate `p`.
    fn max_degree(&self, p: TermId, role: Role) -> u64;
    /// Optional degree summary (Tier-2). Defaults to `None`.
    fn degree_sequence(&self, _p: TermId, _role: Role) -> Option<DegreeSummary> {
        None
    }
    /// Optional sampled join estimate `(estimate, upper_bound)`. Defaults to
    /// `None`.
    fn sample_join(&self, _patterns: &[TriplePattern]) -> Option<(f64, f64)> {
        None
    }
}

/// The zero-stats fallback: no real statistics gathered. Every method returns
/// the most conservative value, so the estimator can never be made worse by
/// fabricating selectivity it does not have.
pub struct ZeroStats {
    total: u64,
    empty_index: CharacteristicSetIndex,
}

impl ZeroStats {
    pub fn new(total: u64) -> Self {
        Self {
            total,
            empty_index: CharacteristicSetIndex::empty(),
        }
    }
}

impl Stats for ZeroStats {
    fn total_triples(&self) -> u64 {
        self.total
    }

    /// No per-predicate knowledge → assume the whole graph.
    fn predicate_count(&self, _p: TermId) -> u64 {
        self.total
    }

    /// Most-conservative denominator: never divides output down spuriously.
    fn ndv(&self, _p: TermId, _pos: Position) -> u64 {
        1
    }

    fn characteristic_sets(&self) -> &CharacteristicSetIndex {
        &self.empty_index
    }

    /// Loosest bound.
    fn max_degree(&self, _p: TermId, _role: Role) -> u64 {
        self.total
    }
}

/// Statistics computed by scanning an immutable [`VecTripleSource`] snapshot
/// once ("recompute-from-snapshot"). This task ships **Tier 0** only: exact
/// per-predicate triple counts and per-position number-of-distinct-values (NDV).
///
/// Tiers 1/2 — characteristic sets and per-predicate `max_degree` — are Task 3.
/// The struct is shaped so those fields drop in next to the Tier-0 maps.
///
/// Exact distinct counts come from an adjacent-dedup over the already-sorted
/// snapshot rows: correct and cheap at snapshot scale, no HyperLogLog needed.
/// (HyperLogLog is the future path for the *incremental* estimator, where rows
/// are not re-scanned.)
pub struct SnapshotStats {
    total: u64,
    /// Predicate -> number of triples with that predicate.
    predicate_count: HashMap<TermId, u64>,
    /// Predicate -> distinct subjects for that predicate.
    ndv_subject: HashMap<TermId, u64>,
    /// Predicate -> distinct objects for that predicate.
    ndv_object: HashMap<TermId, u64>,
    /// Empty for now; Task 3 (Tier 1) fills this from the snapshot.
    characteristic_sets: CharacteristicSetIndex,
}

impl SnapshotStats {
    /// Compute Tier-0 statistics by scanning the pinned snapshot once.
    ///
    /// Uses the `Pso` ordering (rows sorted `(predicate, subject, object)`) for
    /// counts and subject-NDV, and the `Pos` ordering (`(predicate, object,
    /// subject)`) for object-NDV. In both, the predicate is the major axis, so
    /// per-predicate rows form one contiguous run. Distinct subjects/objects are
    /// counted by adjacent-dedup within each run (sorted rows → a value is
    /// distinct exactly when it differs from the previous row's value).
    pub fn from_source(src: &VecTripleSource) -> Self {
        let total = src.total_triples() as u64;

        let mut predicate_count: HashMap<TermId, u64> = HashMap::new();
        let mut ndv_subject: HashMap<TermId, u64> = HashMap::new();
        let mut ndv_object: HashMap<TermId, u64> = HashMap::new();

        // Pso: (predicate, subject, object). Predicate = .0 (major run key),
        // subject = .1 (the value we dedup within a run). One pass yields both
        // the per-predicate triple count and the distinct-subject count.
        if let Some(rows) = src.sorted_rows(Ordering::Pso) {
            let mut i = 0;
            while i < rows.len() {
                let p = rows[i].0;
                let mut count = 0u64;
                let mut distinct_s = 0u64;
                let mut prev_s: Option<TermId> = None;
                while i < rows.len() && rows[i].0 == p {
                    count += 1;
                    let s = rows[i].1;
                    if prev_s != Some(s) {
                        distinct_s += 1;
                        prev_s = Some(s);
                    }
                    i += 1;
                }
                predicate_count.insert(p, count);
                ndv_subject.insert(p, distinct_s);
            }
        }

        // Pos: (predicate, object, subject). Predicate = .0, object = .1.
        if let Some(rows) = src.sorted_rows(Ordering::Pos) {
            let mut i = 0;
            while i < rows.len() {
                let p = rows[i].0;
                let mut distinct_o = 0u64;
                let mut prev_o: Option<TermId> = None;
                while i < rows.len() && rows[i].0 == p {
                    let o = rows[i].1;
                    if prev_o != Some(o) {
                        distinct_o += 1;
                        prev_o = Some(o);
                    }
                    i += 1;
                }
                ndv_object.insert(p, distinct_o);
            }
        }

        Self {
            total,
            predicate_count,
            ndv_subject,
            ndv_object,
            characteristic_sets: CharacteristicSetIndex::empty(),
        }
    }
}

impl Stats for SnapshotStats {
    fn total_triples(&self) -> u64 {
        self.total
    }

    /// Exact count for a known predicate; an absent predicate has no triples, so
    /// `0`. (Callers only query predicates that appear in the snapshot; the `0`
    /// is a safe fallback, not an estimator denominator.)
    fn predicate_count(&self, p: TermId) -> u64 {
        self.predicate_count.get(&p).copied().unwrap_or(0)
    }

    /// Exact distinct-value count for a known predicate/position. Absent → `1`,
    /// the most-conservative denominator (never divides output down spuriously,
    /// never divides by zero).
    fn ndv(&self, p: TermId, pos: Position) -> u64 {
        let map = match pos {
            Position::Subject => &self.ndv_subject,
            Position::Object => &self.ndv_object,
        };
        map.get(&p).copied().unwrap_or(1)
    }

    fn characteristic_sets(&self) -> &CharacteristicSetIndex {
        &self.characteristic_sets
    }

    /// Tier 2, Task 3. Until then, the loosest bound: the whole graph.
    fn max_degree(&self, _p: TermId, _role: Role) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Triple;

    #[test]
    fn zero_stats_is_conservative() {
        let total = 100u64;
        let stats = ZeroStats::new(total);

        assert_eq!(stats.total_triples(), total);
        // No per-predicate knowledge → assume the whole graph.
        assert_eq!(stats.predicate_count(7), total);
        // Most-conservative denominator.
        assert_eq!(stats.ndv(7, Position::Subject), 1);
        assert_eq!(stats.ndv(7, Position::Object), 1);
        // Empty characteristic-set index.
        let cs = stats.characteristic_sets();
        assert!(cs.sets.is_empty());
        assert_eq!(cs.residual_subjects, 0);
        assert!(cs.residual_pred_occ.is_empty());
        assert!(cs.by_predicate.is_empty());
        // Loosest degree bound.
        assert_eq!(stats.max_degree(7, Role::Subject), total);
        assert_eq!(stats.max_degree(7, Role::Object), total);
        // Trait defaults.
        assert!(stats.degree_sequence(7, Role::Subject).is_none());
        assert!(stats.sample_join(&[]).is_none());
    }

    #[test]
    fn snapshot_stats_tier0() {
        // p1 (=10): subjects 1,2,3 each with 2 distinct objects drawn from
        // {100,101} → 6 triples, distinct subjects = 3, distinct objects = 2.
        // p2 (=20): subject 1 with object 200 → 1 triple, ndv_s = ndv_o = 1.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(2, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(3, 10, 100),
            Triple::new(3, 10, 101),
            Triple::new(1, 20, 200),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src);

        assert_eq!(stats.total_triples(), 7);
        assert_eq!(stats.predicate_count(10), 6);
        assert_eq!(stats.predicate_count(20), 1);
        assert_eq!(stats.ndv(10, Position::Subject), 3);
        assert_eq!(stats.ndv(10, Position::Object), 2);
        assert_eq!(stats.ndv(20, Position::Subject), 1);
        assert_eq!(stats.ndv(20, Position::Object), 1);

        // Absent predicate: no triples → count 0; NDV falls back to the
        // conservative 1 (never a zero denominator).
        assert_eq!(stats.predicate_count(999), 0);
        assert_eq!(stats.ndv(999, Position::Subject), 1);
        assert_eq!(stats.ndv(999, Position::Object), 1);

        // Tier-1 field is empty until Task 3.
        assert!(stats.characteristic_sets().sets.is_empty());
    }
}
