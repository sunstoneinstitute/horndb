//! Layered read-only statistics seam.
//!
//! A later cardinality estimator (SPEC-23 Phase 3) reads from these types to
//! bound query output sizes. This module defines the [`Stats`] trait, its data
//! types, and [`ZeroStats`] — the deliberately conservative fallback used when
//! no real statistics have been gathered yet.

use crate::ids::TermId;
use crate::pattern::TriplePattern;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
