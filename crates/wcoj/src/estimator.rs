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
//! Task 5 adds two pieces on top of the Task-4 denominator model:
//! - a **sound** `upper_bound` — the cross product of per-pattern maxima
//!   ([`StatsEstimator::pattern_upper`]), which the true join size can never
//!   exceed (each pattern contributes at most its own maximum).
//! - a **Characteristic-Sets** point estimate for a pure subject-star BGP
//!   ([`StatsEstimator::cs_star_estimate`]); every other shape keeps the
//!   denominator estimate.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use crate::ids::TermId;
use crate::pattern::{Term, TriplePattern, Var};
use crate::stats::{CharacteristicSet, CharacteristicSetIndex, Estimate, Position, Stats};

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

    /// Sound per-pattern upper bound: the most rows one pattern can match.
    ///
    /// Predicate bound:
    /// - both endpoints variable → the exact predicate count.
    /// - subject bound, object variable → `max_degree(p, Subject)` (the most
    ///   objects any single subject has on `p`).
    /// - subject variable, object bound → `max_degree(p, Object)` (the most
    ///   subjects any single object has on `p`).
    /// - both bound → `1` (an existence check matches at most one triple).
    ///
    /// Predicate unbound → the whole graph. Floored at 1.
    fn pattern_upper(&self, pat: &TriplePattern) -> u64 {
        let s_bound = pat.s.as_bound().is_some();
        let o_bound = pat.o.as_bound().is_some();
        let upper = match pat.p.as_bound() {
            Some(pid) => match (s_bound, o_bound) {
                (false, false) => self.stats.predicate_count(pid),
                (true, false) => self.stats.max_degree(pid, Position::Subject),
                (false, true) => self.stats.max_degree(pid, Position::Object),
                (true, true) => 1,
            },
            None => self.stats.total_triples(),
        };
        upper.max(1)
    }

    /// Estimate for a single pattern. The expected size is the per-pattern base
    /// (a mean fan-out); the upper bound is the per-pattern maximum. `max >=
    /// mean`, so `estimate <= upper_bound` holds; the `min` clamps it regardless.
    pub fn estimate_pattern(&self, pat: &TriplePattern) -> Estimate {
        let base = self.pattern_base(pat);
        let upper_bound = self.pattern_upper(pat);
        Estimate {
            estimate: base.min(upper_bound).max(1),
            upper_bound,
        }
    }

    /// Estimate the output size of a BGP.
    ///
    /// Empty → join identity `(1, 1)`. Single → [`Self::estimate_pattern`].
    /// Otherwise:
    /// - `upper_bound` is the cross product of the per-pattern maxima
    ///   ([`Self::pattern_upper`]). The join keeps only tuples each pattern can
    ///   match, so it never exceeds their product — a sound bound.
    /// - `estimate` routes on shape: a **pure star** (all patterns share one
    ///   subject variable, every predicate bound) goes to the Characteristic-Sets
    ///   point estimate ([`Self::cs_star_estimate`]); every other shape keeps the
    ///   denominator join model ([`Self::denominator_estimate`]). Multi-star and
    ///   mixed decompositions are deferred to the join-planning phase.
    /// - `estimate` is clamped to `[1, upper_bound]` on both paths.
    ///
    /// Result memoized by a content hash of the ordered pattern slice.
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

        // Part A: sound upper bound = cross product of per-pattern maxima.
        let upper_bound = patterns
            .iter()
            .map(|p| self.pattern_upper(p))
            .fold(1u64, |acc, u| acc.saturating_mul(u))
            .max(1);

        // Route by shape: pure star → CS estimate; else denominator model.
        let raw = match detect_pure_star(patterns) {
            Some(star) => self.cs_star_estimate(&star, upper_bound),
            None => self.denominator_estimate(patterns),
        };
        let estimate = raw.clamp(1, upper_bound);

        let result = Estimate {
            estimate,
            upper_bound,
        };
        self.memo.borrow_mut().insert(key, result);
        result
    }

    /// Denominator join model (Task 4): multiply per-pattern bases, divide once
    /// per variable shared across >= 2 patterns (transitive equality class — one
    /// denominator per variable), then apply a PK/FK cap for key-like shared
    /// variables. Returns the raw estimate (floored at 1); the caller clamps it
    /// to the sound upper bound.
    fn denominator_estimate(&self, patterns: &[TriplePattern]) -> u64 {
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
        estimate.max(1)
    }

    /// Characteristic-Sets point estimate for a pure subject-star (Neumann &
    /// Moerkotte, ICDE 2011 — the standard multiplicity estimate).
    ///
    /// For the star's distinct bound-predicate set `P`:
    /// `est = Σ_{C : P ⊆ C} count(C) · Π_{p in P} factor(C, p)`.
    /// `factor(C, p)` for a **variable** object is the mean objects-per-subject
    /// `occ(C,p)/count(C)`; for a **bound** object it is
    /// `min(1, occ(C,p)/count(C)/ndv(p,Object))` — the chance a subject in `C`
    /// links to that one specific object on `p`.
    ///
    /// A residual term covers the folded tail: if every star predicate appears in
    /// the tail, add `residual_subjects · Π (residual_occ(p)/residual_subjects)`
    /// (bound-object predicates divide by `ndv(p,Object)`, capped at the mean).
    /// This is coarse — the folded tail keeps only aggregate per-predicate
    /// occurrences, not per-set composition, so it treats the tail as one average
    /// set. Clamped to `[1, upper_bound]`.
    fn cs_star_estimate(&self, star: &[(TermId, Term)], upper_bound: u64) -> u64 {
        let index = self.stats.characteristic_sets();
        let preds: Vec<TermId> = star.iter().map(|(p, _)| *p).collect();

        let mut est = 0f64;
        for cs in &index.sets {
            if !sorted_subset(&preds, &cs.predicates) {
                continue;
            }
            let count = cs.count as f64;
            let mut factor = 1f64;
            for &(p, obj) in star {
                let m = occ_in_set(cs, p) as f64 / count;
                factor *= match obj {
                    Term::Var(_) => m,
                    Term::Bound(_) => (m / self.stats.ndv(p, Position::Object) as f64).min(1.0),
                };
            }
            est += count * factor;
        }

        // Residual (tail) term — coarse mean-based approximation, included only
        // when every star predicate is present in the folded tail.
        if index.residual_subjects > 0 {
            let subjects = index.residual_subjects as f64;
            let mut factor = 1f64;
            let mut all_present = true;
            for &(p, obj) in star {
                match residual_occ(index, p) {
                    Some(occ) => {
                        let m = occ as f64 / subjects;
                        factor *= match obj {
                            Term::Var(_) => m,
                            Term::Bound(_) => {
                                (m / self.stats.ndv(p, Position::Object) as f64).min(m)
                            }
                        };
                    }
                    None => {
                        all_present = false;
                        break;
                    }
                }
            }
            if all_present {
                est += subjects * factor;
            }
        }

        (est.round() as u64).clamp(1, upper_bound)
    }

    /// Test-only: the raw denominator-model estimate, bypassing star routing, so
    /// a test can compare the denominator model against the CS estimate on the
    /// same star BGP.
    #[cfg(test)]
    pub fn estimate_bgp_denominator_only(&self, patterns: &[TriplePattern]) -> u64 {
        self.denominator_estimate(patterns)
    }

    #[cfg(test)]
    pub fn memo_len(&self) -> usize {
        self.memo.borrow().len()
    }
}

/// Is `sub` a subset of `sup`? Both are sorted, distinct `TermId` slices.
fn sorted_subset(sub: &[TermId], sup: &[TermId]) -> bool {
    let mut j = 0usize;
    for &x in sub {
        while j < sup.len() && sup[j] < x {
            j += 1;
        }
        if j >= sup.len() || sup[j] != x {
            return false;
        }
        j += 1;
    }
    true
}

/// Object occurrences for predicate `p` within set `cs` (0 if absent).
/// `cs.occurrences` is sorted by predicate.
fn occ_in_set(cs: &CharacteristicSet, p: TermId) -> u64 {
    cs.occurrences
        .binary_search_by_key(&p, |(pp, _)| *pp)
        .map(|i| cs.occurrences[i].1)
        .unwrap_or(0)
}

/// Object occurrences for predicate `p` in the residual tail, if present.
/// `residual_pred_occ` is sorted by predicate.
fn residual_occ(index: &CharacteristicSetIndex, p: TermId) -> Option<u64> {
    index
        .residual_pred_occ
        .binary_search_by_key(&p, |(pp, _)| *pp)
        .ok()
        .map(|i| index.residual_pred_occ[i].1)
}

/// Detect a pure subject-star: >= 2 patterns, all with the same subject variable,
/// every predicate bound. Returns the star's distinct predicates (sorted) paired
/// with each predicate's object term (first occurrence kept if a predicate
/// repeats — the repeated star collapses to its distinct predicates). `None` if
/// the BGP is not a pure star.
fn detect_pure_star(patterns: &[TriplePattern]) -> Option<Vec<(TermId, Term)>> {
    if patterns.len() < 2 {
        return None;
    }
    let subj = patterns[0].s.as_var()?;
    let mut preds: Vec<(TermId, Term)> = Vec::new();
    for pat in patterns {
        if pat.s.as_var() != Some(subj) {
            return None;
        }
        let pid = pat.p.as_bound()?;
        if !preds.iter().any(|(p, _)| *p == pid) {
            preds.push((pid, pat.o));
        }
    }
    preds.sort_by_key(|(p, _)| *p);
    Some(preds)
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
        // This BGP is a pure star, so `estimate_bgp` now routes it to the CS
        // estimator. Test the denominator model itself via the direct path.
        let joined = est.estimate_bgp_denominator_only(&patterns);

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
                                                                 // Pure star → routes to CS in `estimate_bgp`; the PK/FK cap is a
                                                                 // denominator-model feature, so exercise it via the direct path.
        let joined = est.estimate_bgp_denominator_only(&patterns);

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

    /// Backtracking join over the raw triple list — the ground-truth row count
    /// for a BGP. Bound terms must match exactly; a variable binds on first use
    /// and must be consistent everywhere it recurs (including within a triple).
    fn brute_force_count(triples: &[Triple], patterns: &[TriplePattern]) -> u64 {
        fn rec(
            triples: &[Triple],
            patterns: &[TriplePattern],
            idx: usize,
            binding: &mut HashMap<Var, u64>,
        ) -> u64 {
            if idx == patterns.len() {
                return 1;
            }
            let pat = &patterns[idx];
            let mut total = 0u64;
            'next: for t in triples {
                let mut newly: Vec<Var> = Vec::new();
                for (term, val) in [(pat.s, t.s), (pat.p, t.p), (pat.o, t.o)] {
                    match term {
                        Term::Bound(b) => {
                            if b != val {
                                for v in newly.drain(..) {
                                    binding.remove(&v);
                                }
                                continue 'next;
                            }
                        }
                        Term::Var(v) => match binding.get(&v) {
                            Some(&existing) => {
                                if existing != val {
                                    for v in newly.drain(..) {
                                        binding.remove(&v);
                                    }
                                    continue 'next;
                                }
                            }
                            None => {
                                binding.insert(v, val);
                                newly.push(v);
                            }
                        },
                    }
                }
                total += rec(triples, patterns, idx + 1, binding);
                for v in newly {
                    binding.remove(&v);
                }
            }
            total
        }
        let mut binding = HashMap::new();
        rec(triples, patterns, 0, &mut binding)
    }

    /// Correlated graph: 5 subjects each with 2 objects on pred 10 and 3 on pred
    /// 20 (so every pred-10 subject also has pred 20 — an implicit "type"), plus
    /// 10 noise subjects that have ONLY pred 10. The noise inflates
    /// `predicate_count(10)` and `ndv(10,Subject)`, which pulls the independence
    /// (denominator) estimate away from the true star count; the CS estimate,
    /// which reads the exact `{10,20}` set, is unaffected.
    fn correlated_graph() -> Vec<Triple> {
        let mut triples = Vec::new();
        for s in 1..=5u64 {
            triples.push(Triple::new(s, 10, s * 100));
            triples.push(Triple::new(s, 10, s * 100 + 1));
            triples.push(Triple::new(s, 20, s * 200));
            triples.push(Triple::new(s, 20, s * 200 + 1));
            triples.push(Triple::new(s, 20, s * 200 + 2));
        }
        for s in 100..110u64 {
            triples.push(Triple::new(s, 10, s * 100));
        }
        triples
    }

    #[test]
    fn cs_beats_denominator_on_star() {
        let triples = correlated_graph();
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        // ?s <10> ?o1 . ?s <20> ?o2  — pure star.
        let star = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];

        let truth = brute_force_count(&triples, &star);
        let cs = est.estimate_bgp(&star).estimate;
        let denom = est.estimate_bgp_denominator_only(&star);
        println!("cs_beats_denominator_on_star: true={truth} cs={cs} denom={denom}");

        // Σ_s deg(s,10)·deg(s,20) = 5·2·3 = 30 (noise subjects have no pred 20).
        assert_eq!(truth, 30);
        let abs_err = |a: u64, b: u64| a.abs_diff(b);
        assert!(
            abs_err(cs, truth) < abs_err(denom, truth),
            "CS estimate {cs} not closer to true {truth} than denominator {denom}"
        );
        assert!(
            cs >= truth / 10 && cs <= truth * 10,
            "CS estimate {cs} not within an order of magnitude of {truth}"
        );
    }

    #[test]
    fn star_estimate_within_order_of_magnitude() {
        let triples = correlated_graph();
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        let star = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        let truth = brute_force_count(&triples, &star);
        let cs = est.estimate_bgp(&star).estimate;
        assert!(
            truth / 10 <= cs && cs <= truth * 10,
            "CS estimate {cs} outside [{}, {}] for true {truth}",
            truth / 10,
            truth * 10
        );
    }

    #[test]
    fn upper_bound_never_below_measured() {
        // pred 10: s1→{100,101}, s2→{100}, s3→{102}  (max subject fan-out 2)
        // pred 20: s1→{200}, s2→{200,201}, s3→{200}  (object 200 shared by 3)
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(2, 10, 100),
            Triple::new(3, 10, 102),
            Triple::new(1, 20, 200),
            Triple::new(2, 20, 200),
            Triple::new(2, 20, 201),
            Triple::new(3, 20, 200),
        ];
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        // A 2-star sharing subject ?s.
        let star = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(2)),
        ];
        // A single bound-subject pattern (exercises max_degree(Subject)).
        let bound_subj = vec![TriplePattern::new(bound(1), bound(10), var(0))];
        // A 2-pattern chain sharing an object var ?o.
        let chain = vec![
            TriplePattern::new(var(0), bound(10), var(2)),
            TriplePattern::new(var(1), bound(20), var(2)),
        ];

        for shape in [&star, &bound_subj, &chain] {
            let truth = brute_force_count(&triples, shape);
            let upper = est.estimate_bgp(shape).upper_bound;
            assert!(
                upper >= truth,
                "upper_bound {upper} below measured {truth} for shape {shape:?}"
            );
        }
    }
}
