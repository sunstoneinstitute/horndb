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

    /// A pattern is *structurally empty* when its predicate is bound but has no
    /// triples in the stats (`predicate_count(pid) == 0`) — e.g. every triple on
    /// that predicate was retracted while the predicate id stays known. Such a
    /// pattern matches exactly zero rows, so any join containing it is empty.
    fn is_structurally_empty(&self, pat: &TriplePattern) -> bool {
        matches!(pat.p.as_bound(), Some(pid) if self.stats.predicate_count(pid) == 0)
    }

    /// Estimate for a single pattern. The expected size is the per-pattern base
    /// (a mean fan-out); the upper bound is the per-pattern maximum. `max >=
    /// mean`, so `estimate <= upper_bound` holds; the `min` clamps it regardless.
    pub fn estimate_pattern(&self, pat: &TriplePattern) -> Estimate {
        // A bound predicate with zero triples is exactly empty — preserve the
        // genuine zero rather than letting the floor-at-1 logic round it up to 1.
        if self.is_structurally_empty(pat) {
            return Estimate {
                estimate: 0,
                upper_bound: 0,
            };
        }
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
    /// Any structurally empty pattern (bound predicate with zero triples) →
    /// `(0, 0)`: a join with an empty relation is empty. Otherwise:
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

        // If any pattern is structurally empty (bound predicate with zero
        // triples), the join is empty. Return the genuine zero before the
        // floor-at-1 logic in the base/product path can round it up to (1, 1).
        if patterns.iter().any(|p| self.is_structurally_empty(p)) {
            return Estimate {
                estimate: 0,
                upper_bound: 0,
            };
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

        // Where each variable appears. `Some(pos)` is a subject/object position;
        // `None` marks a predicate position. A predicate-position variable shared
        // across patterns is also an equality constraint (the two rows must agree
        // on the same predicate value), so it too contributes a denominator — the
        // number of distinct predicates. Occurrences of one variable across
        // predicate and subject/object positions form a single equality class, so
        // they are grouped under the same key and divided once (using the max of
        // the applicable denominators).
        let mut var_occ: HashMap<Var, Vec<(usize, Option<Position>)>> = HashMap::new();
        for (idx, pat) in patterns.iter().enumerate() {
            if let Some(v) = pat.s.as_var() {
                var_occ
                    .entry(v)
                    .or_default()
                    .push((idx, Some(Position::Subject)));
            }
            if let Some(v) = pat.p.as_var() {
                var_occ.entry(v).or_default().push((idx, None));
            }
            if let Some(v) = pat.o.as_var() {
                var_occ
                    .entry(v)
                    .or_default()
                    .push((idx, Some(Position::Object)));
            }
        }

        // Denominator: divide once per variable shared by >= 2 distinct patterns.
        let mut denom_product = 1u64;
        for occs in var_occ.values() {
            let distinct_patterns: HashSet<usize> = occs.iter().map(|(i, _)| *i).collect();
            if distinct_patterns.len() < 2 {
                continue;
            }
            // denom_v = max distinct-count over the binding positions. A
            // subject/object position on a bound predicate uses its per-position
            // ndv (unbound → that pattern's base as a proxy); a predicate position
            // uses the graph's distinct-predicate count.
            let mut denom_v = 1u64;
            for &(idx, pos) in occs {
                let d = match pos {
                    None => self.stats.distinct_predicates(),
                    Some(pos) => match patterns[idx].p.as_bound() {
                        Some(pid) => self.stats.ndv(pid, pos),
                        // Fallback proxy for an unbound predicate. Caveat: for a
                        // both-endpoints-variable pattern the base is
                        // `total_triples()`, so this denominator can over-shrink
                        // the estimate. Acceptable — unbound predicates are rare.
                        None => bases[idx],
                    },
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
            let key_like = occs.iter().any(|&(idx, pos)| match pos {
                // Only a subject/object position can be key-like (a distinct value
                // of the variable in every row of its predicate).
                Some(pos) => match patterns[idx].p.as_bound() {
                    Some(pid) => self.stats.ndv(pid, pos) == self.stats.predicate_count(pid),
                    None => false,
                },
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
/// every predicate bound, every predicate distinct, and **independent objects**.
/// Returns the star's predicates (sorted) paired with each predicate's object
/// term. `None` if the BGP is not such a pure star.
///
/// **Independent objects.** The Characteristic-Sets multiplicity formula
/// multiplies each predicate's object multiplicity *independently*. That is only
/// valid when the object of each pattern varies freely of the others. A shared
/// object variable imposes an equality (the same value must appear on two
/// predicates), and the subject variable recurring in an object position is a
/// self-join — neither is independent. CS would then multiply as if free and can
/// return a large positive estimate when the true answer is tiny (the objects
/// must coincide). So a pattern's object must be either a bound constant, or a
/// variable that (a) is not the star's subject and (b) is unique to its pattern.
/// If any object variable repeats across patterns, equals the subject, or the
/// subject appears in an object position, this returns `None` and the BGP routes
/// to the denominator model in [`StatsEstimator::estimate_bgp`], which models the
/// extra equality join.
///
/// A **repeated predicate** in the star (e.g. `?s p ?o1 . ?s p ?o2`) is a
/// conjunction of two edges on the same predicate, not one edge. The
/// Characteristic-Sets multiplicity formula keys on the *distinct* predicate set,
/// so collapsing the repeat to a single predicate would estimate one edge
/// (~`predicate_count(p)`) instead of the conjunction (≈ `Σ_s deg(s,p)²`) — a
/// large under-count. Such stars deliberately return `None` and route to the
/// denominator model, which handles them correctly: each `?s p ?o_i` pattern
/// contributes `base = predicate_count(p)`, the shared subject variable divides
/// once by `ndv(p,Subject)`, and the `pattern_upper` cross-product stays a sound
/// upper bound.
fn detect_pure_star(patterns: &[TriplePattern]) -> Option<Vec<(TermId, Term)>> {
    if patterns.len() < 2 {
        return None;
    }
    let subj = patterns[0].s.as_var()?;
    let mut preds: Vec<(TermId, Term)> = Vec::new();
    // Object variables already claimed by an earlier pattern — each must be
    // unique to its pattern for the independent-objects requirement.
    let mut object_vars: HashSet<Var> = HashSet::new();
    for pat in patterns {
        if pat.s.as_var() != Some(subj) {
            return None;
        }
        let pid = pat.p.as_bound()?;
        // Any repeated predicate disqualifies the pure-star routing (see above).
        if preds.iter().any(|(p, _)| *p == pid) {
            return None;
        }
        // Independent-objects check: a variable object must not be the subject
        // (self-join) and must not repeat across patterns (equality join).
        if let Some(ov) = pat.o.as_var() {
            if ov == subj {
                return None;
            }
            if !object_vars.insert(ov) {
                return None;
            }
        }
        preds.push((pid, pat.o));
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

    #[test]
    fn repeated_predicate_star_not_collapsed() {
        // Subject 1 has 3 objects on pred 10; subject 2 has 1. So the star
        // `?s 10 ?o1 . ?s 10 ?o2` counts Σ_s deg(s,10)² = 9 + 1 = 10, while
        // predicate_count(10) = Σ_s deg = 4. The two differ, so an estimate that
        // collapsed the repeated predicate to one edge (~predicate_count) is
        // visibly wrong.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(1, 10, 102),
            Triple::new(2, 10, 200),
        ];
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        let bgp = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(10), var(2)),
        ];

        let truth = brute_force_count(&triples, &bgp);
        assert_eq!(truth, 10, "Σ deg² sanity");
        let pred_count = stats.predicate_count(10);
        assert_eq!(pred_count, 4);

        let full = est.estimate_bgp(&bgp);

        // (a) Not the collapsed single-edge estimate (~predicate_count).
        assert!(
            full.estimate > pred_count,
            "estimate {} did not exceed collapsed single-edge count {pred_count}",
            full.estimate
        );
        // (b) Upper bound is sound.
        assert!(
            full.upper_bound >= truth,
            "upper_bound {} below true {truth}",
            full.upper_bound
        );
        // Repeated-predicate star is routed away from the CS collapse.
        assert!(
            detect_pure_star(&bgp).is_none(),
            "repeated-predicate star must not be detected as a pure star"
        );
    }

    #[test]
    fn cs_rejects_shared_object_star() {
        // BGP `?s 10 ?o . ?s 20 ?o` shares the OBJECT variable ?o, so the same
        // object value must appear on both predicates for a subject. The
        // Characteristic-Sets formula multiplies each predicate's object
        // multiplicity independently, which ignores that equality and can
        // over-estimate wildly. Here the two object pools are disjoint, so the
        // true join is 0 — CS would still return a positive count.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(2, 10, 100),
            Triple::new(1, 20, 200),
            Triple::new(1, 20, 201),
            Triple::new(2, 20, 201),
        ];
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        // Var(1) is the shared object variable in both patterns.
        let bgp = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(20), var(1)),
        ];

        // (Y) A shared-object star is NOT a pure star — must route to the
        // denominator model, which handles the extra equality join.
        assert!(
            detect_pure_star(&bgp).is_none(),
            "shared-object star must not be detected as a pure star"
        );

        let full = est.estimate_bgp(&bgp);
        let denom = est.estimate_bgp_denominator_only(&bgp);
        assert_eq!(
            full.estimate,
            denom.clamp(1, full.upper_bound),
            "shared-object star did not route to the denominator model"
        );

        // Upper bound stays sound (the disjoint object pools make truth = 0).
        let truth = brute_force_count(&triples, &bgp);
        assert_eq!(truth, 0, "disjoint object pools → empty join");
        assert!(
            full.upper_bound >= truth,
            "upper_bound {} below true {truth}",
            full.upper_bound
        );
    }

    #[test]
    fn shared_predicate_variable_divides() {
        // BGP `?s ?p ?o . ?x ?p ?y` shares only the PREDICATE variable ?p, with
        // distinct subject/object variables. SPARQL requires the two ?p bindings
        // to be equal, so the estimate must divide the product by the number of
        // distinct predicates — not estimate the full cross product.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(1, 20, 200),
            Triple::new(2, 20, 201),
            Triple::new(3, 30, 300),
        ];
        let stats = stats_of(triples.clone());
        let est = StatsEstimator::new(&stats);

        let bgp = vec![
            TriplePattern::new(var(0), var(1), var(2)),
            TriplePattern::new(var(3), var(1), var(4)),
        ];

        let base = est.estimate_pattern(&bgp[0]).estimate;
        let full = est.estimate_bgp(&bgp);
        assert!(
            full.estimate < base * base,
            "shared ?p contributed no denominator: {} !< {}",
            full.estimate,
            base * base
        );

        let truth = brute_force_count(&triples, &bgp);
        assert!(
            full.upper_bound >= truth,
            "upper_bound {} below true {truth}",
            full.upper_bound
        );
    }

    #[test]
    fn zero_count_predicate_is_exact_zero() {
        // Graph has predicates 10 and 20; predicate 99 is absent (zero triples),
        // modelling a predicate whose triples were all retracted but whose id
        // stays known.
        let stats = stats_of(vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(1, 20, 200),
        ]);
        let est = StatsEstimator::new(&stats);
        assert_eq!(stats.predicate_count(99), 0, "predicate 99 must be absent");

        // Single pattern on the absent predicate → exact zero, not the (1,1) floor.
        let single = vec![TriplePattern::new(var(0), bound(99), var(1))];
        assert_eq!(
            est.estimate_bgp(&single),
            Estimate {
                estimate: 0,
                upper_bound: 0
            }
        );

        // 2-pattern BGP mixing the absent predicate with a present one → still
        // (0,0): a join with an empty relation is empty.
        let mixed = vec![
            TriplePattern::new(var(0), bound(10), var(1)),
            TriplePattern::new(var(0), bound(99), var(2)),
        ];
        assert_eq!(
            est.estimate_bgp(&mixed),
            Estimate {
                estimate: 0,
                upper_bound: 0
            }
        );
    }

    /// SPEC-23 acceptance #3 accuracy gate — an in-crate unit test that holds the
    /// stats-backed estimator to the three claims of acceptance #3 on a
    /// representative shape suite over a synthetic graph with realistic predicate
    /// correlation (implicit types). It reuses the crate-internal ground-truth
    /// oracle ([`brute_force_count`]) and the denominator-only path
    /// ([`StatsEstimator::estimate_bgp_denominator_only`]).
    ///
    /// **Representative graph** (`representative_graph`):
    /// - **Type A** — subjects `1..=20`, each with predicates `{p1, p2, p3}` and
    ///   fan-outs `p1 → 2` objects, `p2 → 1`, `p3 → 3`. So every subject that has
    ///   `p1` also has `p2` and `p3` — an implicit "type A" correlation the
    ///   independence model cannot see.
    /// - **Type B** — subjects `100..=109`, each with `{p1, p4}` and fan-outs
    ///   `p1 → 2`, `p4 → 2`. `p1` is shared with A; `p4` is exclusive to B.
    /// - Object value pools are disjoint per predicate, so per-position NDV is
    ///   meaningful. Subject/predicate/object id ranges are chosen not to collide.
    ///
    /// The correlation makes the independence (uniform / denominator) model
    /// measurably wrong: `predicate_count(p1)` and `ndv(p1,Subject)` are inflated
    /// by the type-B subjects, which the per-predicate independence product cannot
    /// discount. The Characteristic-Sets estimator reads the exact `{p1,p2,p3}` /
    /// `{p1,p4}` sets and is unaffected.
    ///
    /// **Skewed regime** (Type C/D/E). The A/B graph is regular (uniform fan-out),
    /// so CS is *exact* there — which alone cannot tell a correct estimator from
    /// one that only happens to be exact on uniform fan-out. To exercise CS's
    /// mean-based *approximation*, the graph also carries a skewed, positively
    /// correlated star population:
    /// - **Type C** — subjects `200..=219` on `{p5, p6}` with highly variable
    ///   fan-out: 3 "hub" subjects have 20 objects on each of `p5` and `p6`; the
    ///   other 17 have 1 each. `p5` and `p6` fan-outs move together (correlation),
    ///   so the true star count is dominated by the hubs and the mean-based CS
    ///   formula `count · mean(p5) · mean(p6)` under-estimates it (`err > 0`).
    /// - **Type D** — 50 subjects with only `p5`: global noise that inflates
    ///   `predicate_count(p5)` / `ndv(p5,Subject)` but not the `{p5,p6}` set, so it
    ///   hurts the denominator model while CS ignores it.
    /// - **Type E** — a blob on an unrelated predicate `p7` that inflates
    ///   `total_triples`, so the total-based `UniformEstimator` over-estimates the
    ///   `p5`/`p6` star while the per-predicate CS/denominator stats stay untouched.
    #[cfg(test)]
    mod accuracy_gate {
        use super::*;
        use crate::cardinality::{Cardinality, UniformEstimator};

        // Predicate ids. p1 is shared across A/B; p2/p3 are Type-A only; p4 is
        // Type-B only. p5/p6 are the skewed correlated star (Type C); p7 is the
        // unrelated total-inflating blob (Type E).
        const P1: u64 = 1000;
        const P2: u64 = 1001;
        const P3: u64 = 1002;
        const P4: u64 = 1003;
        const P5: u64 = 1004;
        const P6: u64 = 1005;
        const P7: u64 = 1006;

        /// The representative correlated graph documented on the module.
        fn representative_graph() -> Vec<Triple> {
            let mut t = Vec::new();
            // Type A: subjects 1..=20 with {p1:2, p2:1, p3:3}.
            for s in 1..=20u64 {
                t.push(Triple::new(s, P1, 2000 + 2 * s));
                t.push(Triple::new(s, P1, 2000 + 2 * s + 1));
                t.push(Triple::new(s, P2, 3000 + s));
                t.push(Triple::new(s, P3, 4000 + 3 * s));
                t.push(Triple::new(s, P3, 4000 + 3 * s + 1));
                t.push(Triple::new(s, P3, 4000 + 3 * s + 2));
            }
            // Type B: subjects 100..=109 with {p1:2, p4:2}.
            for s in 100..=109u64 {
                t.push(Triple::new(s, P1, 2000 + 2 * s));
                t.push(Triple::new(s, P1, 2000 + 2 * s + 1));
                t.push(Triple::new(s, P4, 5000 + 2 * s));
                t.push(Triple::new(s, P4, 5000 + 2 * s + 1));
            }
            // Type C: subjects 200..=219 with a highly skewed, correlated {p5,p6}.
            // Hubs 200..=202 carry 20 objects on each of p5 and p6; the other 17
            // subjects carry 1 each. Disjoint high object ranges avoid collision.
            for s in 200..=219u64 {
                let fan = if s <= 202 { 20 } else { 1 };
                for i in 0..fan {
                    t.push(Triple::new(s, P5, 6_000_000 + s * 1000 + i));
                    t.push(Triple::new(s, P6, 7_000_000 + s * 1000 + i));
                }
            }
            // Type D: 50 subjects with ONLY p5 (global p5 noise).
            for s in 300..=349u64 {
                t.push(Triple::new(s, P5, 6_000_000 + s * 1000));
            }
            // Type E: unrelated p7 blob, 100 subjects × 12 objects, to inflate
            // total_triples (hurts UniformEstimator; leaves CS/denom untouched).
            for s in 400..=499u64 {
                for i in 0..12 {
                    t.push(Triple::new(s, P7, 8_000_000 + s * 1000 + i));
                }
            }
            t
        }

        /// Log-ratio error: `|ln(max(est,1) / max(truth,1))|`. Symmetric in
        /// over/under-estimation, so a 10× miss either way scores the same.
        fn err(est: u64, truth: u64) -> f64 {
            ((est.max(1) as f64) / (truth.max(1) as f64)).ln().abs()
        }

        /// Independence (uniform) BGP estimate: the product of the old
        /// per-pattern [`UniformEstimator`] estimates — the pre-stats baseline.
        fn uniform_bgp(u: &UniformEstimator, shape: &[TriplePattern]) -> u64 {
            shape
                .iter()
                .map(|p| u.estimate(p) as u64)
                .product::<u64>()
                .max(1)
        }

        struct Shape {
            name: &'static str,
            pats: Vec<TriplePattern>,
            is_star: bool,
        }

        #[test]
        fn accuracy_gate_spec23_acceptance_3() {
            let triples = representative_graph();
            let stats = stats_of(triples.clone());
            let est = StatsEstimator::new(&stats);
            let src = VecTripleSource::from_triples(triples.clone());
            let uni = UniformEstimator::from_source(&src);

            let s = var(0);
            let shapes = vec![
                // (1) single pattern.
                Shape {
                    name: "1:single ?s p1 ?o",
                    pats: vec![TriplePattern::new(s, bound(P1), var(1))],
                    is_star: false,
                },
                // (2) correlated star — only Type A has both p1 and p2.
                Shape {
                    name: "2:corr-star p1.p2",
                    pats: vec![
                        TriplePattern::new(s, bound(P1), var(1)),
                        TriplePattern::new(s, bound(P2), var(2)),
                    ],
                    is_star: true,
                },
                // (3) cross-type star — only Type B has both p1 and p4.
                Shape {
                    name: "3:cross-star p1.p4",
                    pats: vec![
                        TriplePattern::new(s, bound(P1), var(1)),
                        TriplePattern::new(s, bound(P4), var(2)),
                    ],
                    is_star: true,
                },
                // (4) 3-pattern star — only Type A.
                Shape {
                    name: "4:3-star p1.p2.p3",
                    pats: vec![
                        TriplePattern::new(s, bound(P1), var(1)),
                        TriplePattern::new(s, bound(P2), var(2)),
                        TriplePattern::new(s, bound(P3), var(3)),
                    ],
                    is_star: true,
                },
                // (5) bound-subject pattern (subject 1 is Type A).
                Shape {
                    name: "5:bound-subj <1> p1 ?o",
                    pats: vec![TriplePattern::new(bound(1), bound(P1), var(1))],
                    is_star: false,
                },
                // (6) skewed correlated star — Type C, highly variable fan-out.
                // Exercises CS's mean-based approximation (err > 0), where CS must
                // still beat both uniform and the denominator model.
                Shape {
                    name: "6:skew-star p5.p6",
                    pats: vec![
                        TriplePattern::new(s, bound(P5), var(1)),
                        TriplePattern::new(s, bound(P6), var(2)),
                    ],
                    is_star: true,
                },
            ];

            eprintln!(
                "{:<24} {:>6} {:>8} {:>6} {:>6} {:>8}",
                "shape", "truth", "uniform", "denom", "stats", "upper"
            );

            let mut stats_err_sum = 0.0;
            let mut uni_err_sum = 0.0;
            let mut cs_star_err_sum = 0.0;
            let mut denom_star_err_sum = 0.0;
            let mut within_om = 0usize;
            // Correlated-star (shape 2) errors, for the strict CS-vs-denom check.
            let mut corr_full = f64::NAN;
            let mut corr_denom = f64::NAN;
            // Skewed-star (shape 6) errors, for the approximation-regime checks.
            let mut skew_cs = f64::NAN;
            let mut skew_denom = f64::NAN;
            let mut skew_uni = f64::NAN;

            for sh in &shapes {
                let truth = brute_force_count(&triples, &sh.pats);
                let full = est.estimate_bgp(&sh.pats);
                let stats_est = full.estimate;
                let denom = est.estimate_bgp_denominator_only(&sh.pats);
                let uni_est = uniform_bgp(&uni, &sh.pats);

                eprintln!(
                    "{:<24} {:>6} {:>8} {:>6} {:>6} {:>8}",
                    sh.name, truth, uni_est, denom, stats_est, full.upper_bound
                );

                // (c) upper bound never below measured — every shape.
                assert!(
                    full.upper_bound >= truth,
                    "(c) upper_bound {} < truth {} for {}",
                    full.upper_bound,
                    truth,
                    sh.name
                );

                stats_err_sum += err(stats_est, truth);
                uni_err_sum += err(uni_est, truth);

                // Within an order of magnitude of truth (max/min <= 10).
                let lo = stats_est.min(truth).max(1);
                let hi = stats_est.max(truth).max(1);
                if (hi as f64) / (lo as f64) <= 10.0 {
                    within_om += 1;
                }

                if sh.is_star {
                    cs_star_err_sum += err(stats_est, truth);
                    denom_star_err_sum += err(denom, truth);
                }
                if sh.name.starts_with("2:") {
                    corr_full = err(stats_est, truth);
                    corr_denom = err(denom, truth);
                }
                if sh.name.starts_with("6:") {
                    skew_cs = err(stats_est, truth);
                    skew_denom = err(denom, truth);
                    skew_uni = err(uni_est, truth);
                }
            }

            let n = shapes.len() as f64;
            let star_n = shapes.iter().filter(|s| s.is_star).count() as f64;
            let stats_mean = stats_err_sum / n;
            let uni_mean = uni_err_sum / n;
            let cs_star_mean = cs_star_err_sum / star_n;
            let denom_star_mean = denom_star_err_sum / star_n;

            // (a) Strictly better than UniformEstimator (mean log-ratio error).
            assert!(
                stats_mean < uni_mean,
                "(a) stats mean err {stats_mean} !< uniform mean err {uni_mean}"
            );

            // (b) CS beats the denominator model on star shapes (mean), and
            // strictly on the correlated star (shape 2).
            assert!(
                cs_star_mean <= denom_star_mean,
                "(b) CS star mean err {cs_star_mean} > denom star mean err {denom_star_mean}"
            );
            assert!(
                corr_full < corr_denom,
                "(b-strict) correlated-star full err {corr_full} !< denom err {corr_denom}"
            );

            // Skewed-regime checks (shape 6). Here CS is genuinely approximate,
            // so this proves the gate exercises the real (non-exact) regime and
            // that CS still wins under approximation.
            assert!(
                skew_cs > 0.0,
                "(skew) CS error {skew_cs} is not > 0 — the skewed star is not \
                 exercising CS's approximation (graph too regular?)"
            );
            assert!(
                skew_cs < skew_uni,
                "(skew) CS error {skew_cs} !< uniform error {skew_uni}"
            );
            assert!(
                skew_cs < skew_denom,
                "(skew) CS error {skew_cs} !< denominator error {skew_denom}"
            );

            // Within-order-of-magnitude fraction across ALL shapes. Measured 1.0
            // in this baseline run: shapes 1–5 are exact on the regular A/B graph,
            // and the skewed star (6) — though CS is approximate there — still
            // lands within 10× of truth. 0.8 is the locked gate threshold.
            let frac = within_om as f64 / n;
            assert!(
                frac >= 0.8,
                "within-order-of-magnitude fraction {frac} < locked threshold 0.8"
            );

            eprintln!(
                "means: stats={stats_mean:.4} uniform={uni_mean:.4} | \
                 star: cs={cs_star_mean:.4} denom={denom_star_mean:.4} | \
                 skew: cs={skew_cs:.4} denom={skew_denom:.4} uniform={skew_uni:.4} | \
                 within-1-OoM fraction={frac} (locked threshold 0.8; measured {frac})"
            );
        }
    }
}
