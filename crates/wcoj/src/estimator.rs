//! Cardinality estimator built on the layered [`Stats`] seam (SPEC-23 Phase 3).
//!
//! [`StatsEstimator`] turns read-only statistics into two numbers for a basic
//! graph pattern (BGP): an expected output size (`estimate`) and an
//! `upper_bound` the true size never exceeds. It has two layers:
//!
//! - **Per-pattern base** ([`StatsEstimator::estimate_pattern`]) — how many
//!   triples one pattern matches on its own.
//! - **BGP join model** ([`StatsEstimator::estimate_bgp`]) — multiply the
//!   per-pattern bases, then divide by a denominator for each variable shared
//!   across patterns (the "denominator model" of join estimation). Shared
//!   variables shrink the product because a join keeps only rows that agree on
//!   the shared value.
//!
//! Task 4 scope: the base cardinalities, the denominator join, a PK/FK cap that
//! keeps key joins from exploding, and a provisional `min(product, total)`
//! upper bound. The degree-based upper bound and the Characteristic-Sets point
//! estimate arrive in Task 5.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use crate::pattern::{TriplePattern, Var};
use crate::stats::{Estimate, Position, Stats};

/// Coarse static prior for an unbound-predicate pattern with the subject bound
/// (or, symmetrically, the object bound). Divides the whole graph down by this
/// factor. Tunable later; a placeholder until real predicate-free stats exist.
const UNBOUND_PRED_ONE_SIDE_DIVISOR: u64 = 25;

/// Coarse static prior for an unbound-predicate pattern with both subject and
/// object bound — close to an existence check. Tunable later.
const UNBOUND_PRED_BOTH_DIVISOR: u64 = 1000;

/// Cardinality estimator over a borrowed [`Stats`] source.
///
/// Memoizes BGP estimates by a content hash of the ordered pattern slice, so a
/// repeated `estimate_bgp` over the same patterns is O(1) after the first. The
/// key is the patterns' content, not their count — one estimator can be reused
/// across different BGPs (star sub-groups, EXPLAIN) without cross-BGP collision.
pub struct StatsEstimator<'a, S: Stats> {
    stats: &'a S,
    /// key = 64-bit hash of the ordered `[TriplePattern]` slice.
    memo: RefCell<HashMap<u64, Estimate>>,
}

/// Content hash of an ordered pattern slice, used as the memo key. Distinct
/// pattern sets (different predicates, endpoints, or order) hash to distinct
/// keys with overwhelming probability, so reusing one estimator across BGPs
/// does not return a stale estimate.
fn bgp_key(patterns: &[TriplePattern]) -> u64 {
    let mut hasher = DefaultHasher::new();
    patterns.hash(&mut hasher);
    hasher.finish()
}

impl<'a, S: Stats> StatsEstimator<'a, S> {
    pub fn new(stats: &'a S) -> Self {
        Self {
            stats,
            memo: RefCell::new(HashMap::new()),
        }
    }

    /// Base cardinality of a single pattern, floored at 1.
    ///
    /// Predicate bound: start from the exact predicate count, then divide by the
    /// per-position distinct-value count for each bound endpoint (mean fan-out
    /// per fixed subject/object). Predicate unbound (rare): fall back to coarse
    /// static priors keyed on which endpoints are bound.
    fn pattern_base(&self, pat: &TriplePattern) -> u64 {
        let s_bound = pat.s.as_bound().is_some();
        let o_bound = pat.o.as_bound().is_some();

        let base = match pat.p.as_bound() {
            Some(pid) => {
                let mut b = self.stats.predicate_count(pid);
                if s_bound {
                    // ndv >= 1 by contract → no divide-by-zero.
                    b = (b / self.stats.ndv(pid, Position::Subject)).max(1);
                }
                if o_bound {
                    b = (b / self.stats.ndv(pid, Position::Object)).max(1);
                }
                b
            }
            None => {
                let t = self.stats.total_triples();
                match (s_bound, o_bound) {
                    (false, false) => t,
                    (true, false) | (false, true) => (t / UNBOUND_PRED_ONE_SIDE_DIVISOR).max(1),
                    (true, true) => (t / UNBOUND_PRED_BOTH_DIVISOR).max(1),
                }
            }
        };
        base.max(1)
    }

    /// Estimate for a single pattern. A single scan's expected size is also its
    /// own upper bound, so `estimate == upper_bound`.
    pub fn estimate_pattern(&self, pat: &TriplePattern) -> Estimate {
        let base = self.pattern_base(pat);
        Estimate {
            estimate: base,
            upper_bound: base,
        }
    }

    /// Estimate the output size of a BGP with the denominator join model.
    ///
    /// Empty → join identity `(1, 1)`. Single → [`Self::estimate_pattern`].
    /// Otherwise: multiply per-pattern bases, divide once per shared variable
    /// (transitive equality class — a variable is one class no matter how many
    /// patterns carry it, so its denominator applies exactly once), then apply a
    /// PK/FK cap for key-like shared variables. Result memoized by a content
    /// hash of the ordered pattern slice.
    pub fn estimate_bgp(&self, patterns: &[TriplePattern]) -> Estimate {
        match patterns.len() {
            0 => {
                return Estimate {
                    estimate: 1,
                    upper_bound: 1,
                }
            }
            1 => return self.estimate_pattern(&patterns[0]),
            _ => {}
        }

        // Memo key = content hash of the ordered patterns, so different BGPs of
        // the same length never collide.
        let key = bgp_key(patterns);
        if let Some(cached) = self.memo.borrow().get(&key) {
            return *cached;
        }

        // Per-pattern base cardinalities.
        let bases: Vec<u64> = patterns.iter().map(|p| self.pattern_base(p)).collect();
        let product = bases
            .iter()
            .fold(1u64, |acc, &b| acc.saturating_mul(b))
            .max(1);

        // Where each variable appears at a subject/object position. Predicate-
        // position variables are ignored for the denominator: a shared predicate
        // variable does not join two rows on a shared *value* the way an S/O
        // variable does, so it contributes no denominator here.
        let mut var_occ: HashMap<Var, Vec<(usize, Position)>> = HashMap::new();
        for (idx, pat) in patterns.iter().enumerate() {
            if let Some(v) = pat.s.as_var() {
                var_occ.entry(v).or_default().push((idx, Position::Subject));
            }
            if let Some(v) = pat.o.as_var() {
                var_occ.entry(v).or_default().push((idx, Position::Object));
            }
        }

        // Denominator: divide once per variable shared by >= 2 distinct patterns.
        let mut denom_product = 1u64;
        for occs in var_occ.values() {
            let distinct_patterns: HashSet<usize> = occs.iter().map(|(i, _)| *i).collect();
            if distinct_patterns.len() < 2 {
                continue;
            }
            // denom_v = max distinct-count over the binding patterns. A bound
            // predicate uses its per-position ndv; an unbound predicate falls
            // back to that pattern's base as a distinct-count proxy.
            let mut denom_v = 1u64;
            for &(idx, pos) in occs {
                let d = match patterns[idx].p.as_bound() {
                    Some(pid) => self.stats.ndv(pid, pos),
                    // Fallback proxy for an unbound predicate. Caveat: for a
                    // both-endpoints-variable pattern the base is
                    // `total_triples()`, so this denominator can over-shrink the
                    // estimate. Acceptable — unbound predicates are rare.
                    None => bases[idx],
                };
                denom_v = denom_v.max(d);
            }
            denom_product = denom_product.saturating_mul(denom_v.max(1));
        }

        let mut estimate = (product / denom_product).max(1);

        // PK/FK cap. If a shared variable is key-like in some binding pattern —
        // that pattern's bound predicate has a distinct value of the variable in
        // every row (ndv == predicate_count) — the join can never exceed the
        // smaller of the inputs sharing the variable. Cap per key-like variable.
        for occs in var_occ.values() {
            let distinct_patterns: HashSet<usize> = occs.iter().map(|(i, _)| *i).collect();
            if distinct_patterns.len() < 2 {
                continue;
            }
            let key_like = occs
                .iter()
                .any(|&(idx, pos)| match patterns[idx].p.as_bound() {
                    Some(pid) => self.stats.ndv(pid, pos) == self.stats.predicate_count(pid),
                    None => false,
                });
            if key_like {
                if let Some(min_base) = distinct_patterns.iter().map(|&i| bases[i]).min() {
                    estimate = estimate.min(min_base);
                }
            }
        }
        estimate = estimate.max(1);

        // Provisional upper bound (Task 5 replaces with the degree-based bound):
        // the join never exceeds the cross product, nor the whole graph.
        let upper_bound = product.min(self.stats.total_triples()).max(estimate);

        let result = Estimate {
            estimate,
            upper_bound,
        };
        self.memo.borrow_mut().insert(key, result);
        result
    }

    #[cfg(test)]
    pub fn memo_len(&self) -> usize {
        self.memo.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Triple;
    use crate::pattern::Term;
    use crate::source::vec_source::VecTripleSource;
    use crate::stats::SnapshotStats;

    fn stats_of(triples: Vec<Triple>) -> SnapshotStats {
        let src = VecTripleSource::from_triples(triples);
        SnapshotStats::from_source(&src)
    }

    fn var(n: u8) -> Term {
        Term::Var(Var(n))
    }
    fn bound(id: u64) -> Term {
        Term::Bound(id)
    }

    #[test]
    fn estimate_pattern_matches_counts() {
        // Predicate 10: subjects 1,2,3 each with objects 100,101 → 6 triples,
        // ndv(10,Subject)=3, ndv(10,Object)=2.
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(2, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(3, 10, 100),
            Triple::new(3, 10, 101),
        ]);
        let est = StatsEstimator::new(&stats);

        // (?s, <10>, ?o) → whole predicate count.
        let p_all = TriplePattern::new(var(0), bound(10), var(1));
        assert_eq!(est.estimate_pattern(&p_all).estimate, 6);

        // (<1>, <10>, ?o) → 6 / ndv(10,Subject)=3 = 2.
        let p_subj = TriplePattern::new(bound(1), bound(10), var(1));
        assert_eq!(est.estimate_pattern(&p_subj).estimate, 2);
    }

    #[test]
    fn denominator_join_shrinks() {
        // pred 10: subjects {1,2} × objects {100,101,102} → 6 triples, ndv_s=2.
        // pred 20: subjects {1,2} × objects {200,201,202} → 6 triples, ndv_s=2.
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(1, 10, 102),
            Triple::new(2, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(2, 10, 102),
            Triple::new(1, 20, 200),
            Triple::new(1, 20, 201),
            Triple::new(1, 20, 202),
            Triple::new(2, 20, 200),
            Triple::new(2, 20, 201),
            Triple::new(2, 20, 202),
        ]);
        let est = StatsEstimator::new(&stats);

        // (?s,<10>,?o1) and (?s,<20>,?o2) share ?s.
        let patterns = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        let base1 = est.estimate_pattern(&patterns[0]).estimate;
        let base2 = est.estimate_pattern(&patterns[1]).estimate;
        let joined = est.estimate_bgp(&patterns).estimate;

        assert_eq!(base1, 6);
        assert_eq!(base2, 6);
        assert!(
            base1.max(base2) <= joined,
            "join dropped below larger input"
        );
        assert!(joined <= base1 * base2, "join exceeded cross product");
    }

    #[test]
    fn pkfk_cap() {
        // pred 10: subjects 1,2,3 each ONE object → count 3, ndv_s = 3 → ?s is a
        // key here (ndv == count). pred 20: subjects 1,2,3 each two objects →
        // count 6, ndv_s = 3.
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(3, 10, 102),
            Triple::new(1, 20, 200),
            Triple::new(1, 20, 201),
            Triple::new(2, 20, 202),
            Triple::new(2, 20, 203),
            Triple::new(3, 20, 204),
            Triple::new(3, 20, 205),
        ]);
        let est = StatsEstimator::new(&stats);

        let patterns = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        let base1 = est.estimate_pattern(&patterns[0]).estimate; // 3
        let base2 = est.estimate_pattern(&patterns[1]).estimate; // 6
        let joined = est.estimate_bgp(&patterns).estimate;

        assert_eq!(base1, 3);
        assert_eq!(base2, 6);
        assert!(
            joined <= base1.min(base2),
            "PK/FK cap not applied: {joined} > {}",
            base1.min(base2)
        );
    }

    #[test]
    fn memoized() {
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(2, 10, 100),
            Triple::new(1, 20, 200),
            Triple::new(2, 20, 201),
        ]);
        let est = StatsEstimator::new(&stats);

        let patterns = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        let first = est.estimate_bgp(&patterns);
        let len_after_first = est.memo_len();
        let second = est.estimate_bgp(&patterns);

        assert_eq!(first, second);
        assert_eq!(len_after_first, 1);
        assert_eq!(est.memo_len(), 1, "memo grew on identical repeat call");
    }

    #[test]
    fn distinct_bgps_do_not_collide() {
        // Two predicates with different counts so their single-shared-var joins
        // have different true estimates:
        //   pred 10: subjects {1,2} × objects {100,101,102} → 6 triples, ndv_s=2.
        //   pred 20: subjects {1,2} × objects {200,201}     → 4 triples, ndv_s=2.
        //   pred 30: subjects {1,2} × objects {300}         → 2 triples, ndv_s=2.
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(1, 10, 102),
            Triple::new(2, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(2, 10, 102),
            Triple::new(1, 20, 200),
            Triple::new(1, 20, 201),
            Triple::new(2, 20, 200),
            Triple::new(2, 20, 201),
            Triple::new(1, 30, 300),
            Triple::new(2, 30, 300),
        ]);
        let est = StatsEstimator::new(&stats);

        // Same length (2 patterns), same shape, different predicates → different
        // true estimates. A count-only memo key would return the first for both.
        let bgp_a = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        let bgp_b = vec![
            TriplePattern::new(var(0), bound(20), var(1)),
            TriplePattern::new(var(0), bound(30), var(2)),
        ];

        let a = est.estimate_bgp(&bgp_a).estimate;
        let b = est.estimate_bgp(&bgp_b).estimate;

        // Each BGP keeps its own estimate; no cross-collision.
        assert_ne!(a, b, "distinct BGPs collided on the memo key");
        assert_eq!(a, est.estimate_bgp(&bgp_a).estimate);
        assert_eq!(b, est.estimate_bgp(&bgp_b).estimate);
        assert_eq!(est.memo_len(), 2, "two distinct BGPs must hold two entries");
    }
}
