//! Layered read-only statistics seam.
//!
//! A later cardinality estimator (SPEC-23 Phase 3) reads from these types to
//! bound query output sizes. This module defines the [`Stats`] trait, its data
//! types, and [`ZeroStats`] — the deliberately conservative fallback used when
//! no real statistics have been gathered yet.

use std::collections::HashMap;

use crate::ids::{Ordering, TermId};
use crate::pattern::{Term, TriplePattern, Var};
use crate::source::vec_source::VecTripleSource;

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

/// Cap on how many characteristic sets are kept exactly. Real RDF graphs have a
/// heavy-tailed set distribution — a handful of frequent predicate-sets and a
/// long tail of rare ones. Keeping the top-`CS_TOP_K` by subject count bounds
/// memory; the tail folds into an aggregate residual bucket. `1024` is a
/// data-driven default, tunable later.
pub const CS_TOP_K: usize = 1024;

/// One characteristic set: the exact predicate-set shared by a group of subjects.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// once ("recompute-from-snapshot"). Covers all three tiers:
/// - **Tier 0** — exact per-predicate triple counts and per-position
///   number-of-distinct-values (NDV).
/// - **Tier 1** — the characteristic-set index (top-K predicate-sets + residual).
/// - **Tier 2** — per-(predicate, role) `max_degree`.
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
    /// Tier 1: top-K characteristic sets + residual bucket.
    characteristic_sets: CharacteristicSetIndex,
    /// Tier 2: predicate -> (max subject-role degree, max object-role degree).
    /// See [`SnapshotStats::max_degree`] for the exact role convention.
    max_degree: HashMap<TermId, (u64, u64)>,
    /// `Spo`-sorted snapshot rows `(subject, predicate, object)`, retained so the
    /// Tier-3 [`SnapshotStats::sample_join`] hook can draw index-walk samples.
    /// This is a full copy of the graph. It is populated **only** when sampling is
    /// turned on (see [`SnapshotStats::with_sampling`]); the default path keeps it
    /// empty so building `SnapshotStats` allocates no per-graph copy.
    sample_rows: Vec<(TermId, TermId, TermId)>,
    /// Whether the Tier-3 sampling hook is active. `false` by default; flip it on
    /// with [`SnapshotStats::with_sampling`].
    sampling_enabled: bool,
}

/// Number of index-walk samples the Tier-3 hook draws. Small and fixed: this is a
/// light approximation, not a full Wander Join, and the inner join-count scan is
/// `O(sample_rows)` per sample.
const SAMPLE_K: usize = 64;

impl SnapshotStats {
    /// Compute all three statistics tiers by scanning the pinned snapshot once
    /// per ordering.
    ///
    /// Tier 0 uses the `Pso` ordering (rows sorted `(predicate, subject,
    /// object)`) for counts and subject-NDV, and the `Pos` ordering
    /// (`(predicate, object, subject)`) for object-NDV. In both, the predicate is
    /// the major axis, so per-predicate rows form one contiguous run. Distinct
    /// subjects/objects are counted by adjacent-dedup within each run (sorted rows
    /// → a value is distinct exactly when it differs from the previous row's
    /// value).
    ///
    /// Tier 1 scans the `Spo` ordering (`(subject, predicate, object)`); each
    /// subject's triples form one contiguous run, from which the subject's
    /// distinct-predicate set (its characteristic set) and per-predicate object
    /// counts are read. Tier 2 reuses the `Pso`/`Pos` runs to find each
    /// predicate's largest single-node fan-out.
    pub fn from_source(src: &VecTripleSource) -> Self {
        // Count the deduplicated indexed rows, not `src.total_triples()` (the
        // pre-dedup input length). Self-consistency: `total` must equal the rows
        // the executor can produce and the sum of the exact per-predicate counts,
        // so duplicate input triples never inflate unbound-predicate estimates.
        let total = src
            .sorted_rows(Ordering::Spo)
            .map(|r| r.len() as u64)
            .unwrap_or(0);

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

        let characteristic_sets = Self::build_characteristic_sets(src);
        let max_degree = Self::build_max_degree(src);

        // Default path: retain NO rows. The Tier-3 sampling hook is off, so the
        // per-graph copy is only made when `with_sampling(src, true)` turns it on.
        Self {
            total,
            predicate_count,
            ndv_subject,
            ndv_object,
            characteristic_sets,
            max_degree,
            sample_rows: Vec::new(),
            sampling_enabled: false,
        }
    }

    /// Enable or disable the Tier-3 Wander-Join-style sampling hook
    /// ([`SnapshotStats::sample_join`]). Off by default: sampling carries a
    /// per-query cost and variance, so it is a fallback, not the default
    /// estimator path. Nothing in the estimator consumes this hook today.
    ///
    /// The full copy of the `Spo` snapshot rows the hook samples is made **only
    /// when `enabled` is true** — the default (sampling-off) path allocates no
    /// per-graph copy. Passing `enabled = false` clears any retained rows.
    pub fn with_sampling(mut self, src: &VecTripleSource, enabled: bool) -> Self {
        self.sampling_enabled = enabled;
        self.sample_rows = if enabled {
            src.sorted_rows(Ordering::Spo)
                .map(<[_]>::to_vec)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self
    }

    /// Tier 1: build the characteristic-set index from the `Spo` ordering.
    ///
    /// Rows are `(subject, predicate, object)` sorted, so all triples of one
    /// subject are contiguous, and within that its predicates are contiguous and
    /// sorted. For each subject we read its distinct-predicate set (the "key")
    /// and, per predicate, how many objects it has. Subjects with the same key
    /// are aggregated: `count` = number of such subjects; `occurrences[p]` = sum
    /// of their per-subject object counts on `p`. `occurrences[p] / count` is the
    /// mean objects-per-subject for `p` within the set, used by the star
    /// estimator.
    fn build_characteristic_sets(src: &VecTripleSource) -> CharacteristicSetIndex {
        match src.sorted_rows(Ordering::Spo) {
            Some(rows) => Self::build_characteristic_sets_with_k(rows, CS_TOP_K),
            None => CharacteristicSetIndex::empty(),
        }
    }

    /// Core of [`SnapshotStats::build_characteristic_sets`], parameterised by the
    /// top-K cap so tests can exercise the residual-folding path with a small `k`.
    /// `rows` must be the `Spo`-sorted snapshot rows `(subject, predicate,
    /// object)`. The production path calls this with [`CS_TOP_K`].
    fn build_characteristic_sets_with_k(
        rows: &[(TermId, TermId, TermId)],
        k: usize,
    ) -> CharacteristicSetIndex {
        // key (sorted distinct predicates) -> (subject count, occurrences aligned
        // with the key's predicate order).
        let mut agg: HashMap<Vec<TermId>, (u64, Vec<u64>)> = HashMap::new();

        let mut i = 0;
        while i < rows.len() {
            let s = rows[i].0;
            // Walk this subject's run, collecting (predicate, object-count) in the
            // sorted predicate order the Spo scan yields.
            let mut preds: Vec<TermId> = Vec::new();
            let mut obj_counts: Vec<u64> = Vec::new();
            while i < rows.len() && rows[i].0 == s {
                let p = rows[i].1;
                let mut objs = 0u64;
                while i < rows.len() && rows[i].0 == s && rows[i].1 == p {
                    // Triples are unique, so each row on (s, p) is a distinct object.
                    objs += 1;
                    i += 1;
                }
                preds.push(p);
                obj_counts.push(objs);
            }

            let entry = agg
                .entry(preds)
                .or_insert_with(|| (0, vec![0; obj_counts.len()]));
            entry.0 += 1;
            for (slot, add) in entry.1.iter_mut().zip(obj_counts.iter()) {
                *slot += *add;
            }
        }

        // Materialise every aggregated set, then keep the top-K by subject count
        // and fold the rest into the residual bucket.
        let mut all: Vec<CharacteristicSet> = agg
            .into_iter()
            .map(|(predicates, (count, sums))| {
                let occurrences = predicates.iter().copied().zip(sums).collect();
                CharacteristicSet {
                    predicates,
                    count,
                    occurrences,
                }
            })
            .collect();
        // Descending by count; ties broken by predicate-set for a stable order.
        all.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.predicates.cmp(&b.predicates))
        });

        let retained = all.len().min(k);
        let tail = all.split_off(retained);
        let sets = all;

        let mut residual_subjects = 0u64;
        let mut residual: HashMap<TermId, u64> = HashMap::new();
        for cs in tail {
            residual_subjects += cs.count;
            for (p, occ) in cs.occurrences {
                *residual.entry(p).or_insert(0) += occ;
            }
        }
        let mut residual_pred_occ: Vec<(TermId, u64)> = residual.into_iter().collect();
        residual_pred_occ.sort_unstable_by_key(|(p, _)| *p);

        // Index only the retained sets: predicate -> indices of sets containing it.
        let mut by_predicate: HashMap<TermId, Vec<usize>> = HashMap::new();
        for (idx, cs) in sets.iter().enumerate() {
            for &p in &cs.predicates {
                by_predicate.entry(p).or_default().push(idx);
            }
        }

        CharacteristicSetIndex {
            sets,
            residual_subjects,
            residual_pred_occ,
            by_predicate,
        }
    }

    /// Tier 2: per-predicate maximum single-node fan-out on each role.
    ///
    /// Role convention (easy to get backwards): the *Subject* role degree is
    /// keyed by subject and counts that subject's distinct objects — the largest
    /// object fan-out of any one subject on `p`. It is read from the `Pso`
    /// ordering `(predicate, subject, object)`. The *Object* role degree is keyed
    /// by object and counts distinct subjects — the largest subject fan-out of
    /// any one object on `p` — read from the `Pos` ordering `(predicate, object,
    /// subject)`. Within a `(predicate, key)` group the third axis is sorted, so
    /// distinct values are counted by adjacent-dedup.
    fn build_max_degree(src: &VecTripleSource) -> HashMap<TermId, (u64, u64)> {
        let mut max_degree: HashMap<TermId, (u64, u64)> = HashMap::new();

        // Pso: max object fan-out per subject → the Subject-role degree (.0).
        if let Some(rows) = src.sorted_rows(Ordering::Pso) {
            let mut i = 0;
            while i < rows.len() {
                let p = rows[i].0;
                let mut max_fanout = 0u64;
                while i < rows.len() && rows[i].0 == p {
                    let s = rows[i].1;
                    // Distinct objects for this (p, s): rows are unique so each
                    // row is a distinct object.
                    let mut fanout = 0u64;
                    while i < rows.len() && rows[i].0 == p && rows[i].1 == s {
                        fanout += 1;
                        i += 1;
                    }
                    max_fanout = max_fanout.max(fanout);
                }
                max_degree.entry(p).or_insert((0, 0)).0 = max_fanout;
            }
        }

        // Pos: max subject fan-out per object → the Object-role degree (.1).
        if let Some(rows) = src.sorted_rows(Ordering::Pos) {
            let mut i = 0;
            while i < rows.len() {
                let p = rows[i].0;
                let mut max_fanout = 0u64;
                while i < rows.len() && rows[i].0 == p {
                    let o = rows[i].1;
                    let mut fanout = 0u64;
                    while i < rows.len() && rows[i].0 == p && rows[i].1 == o {
                        fanout += 1;
                        i += 1;
                    }
                    max_fanout = max_fanout.max(fanout);
                }
                max_degree.entry(p).or_insert((0, 0)).1 = max_fanout;
            }
        }

        max_degree
    }

    /// Deterministic seed for the sampling walk, mixed from the patterns' bound
    /// term ids and variable positions. Same BGP → same seed → same estimate, so
    /// the hook is reproducible and tests are stable. No `rand` crate, no clock.
    fn sample_seed(patterns: &[TriplePattern]) -> u64 {
        let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut mix = |v: u64| {
            h ^= v.wrapping_add(0x9e37_79b9_7f4a_7c15);
            h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
            h ^= h >> 33;
        };
        for pat in patterns {
            for (slot, t) in [pat.s, pat.p, pat.o].into_iter().enumerate() {
                match t {
                    Term::Bound(id) => mix(id << 2 | slot as u64),
                    Term::Var(Var(v)) => mix((v as u64) << 2 | slot as u64 | 0x8000_0000),
                }
            }
        }
        h
    }

    /// One step of a 64-bit linear congruential generator (LCG). Cheap,
    /// deterministic, and good enough to spread index-walk samples.
    fn lcg_next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state
    }

    /// `true` if a snapshot row `(s, p, o)` matches the pattern's bound slots.
    /// Variables (including repeated ones) are treated as wildcards — this is a
    /// light hook, not exact matching.
    fn row_matches(pat: &TriplePattern, row: (TermId, TermId, TermId)) -> bool {
        let ok = |t: Term, v: TermId| match t {
            Term::Bound(b) => b == v,
            Term::Var(_) => true,
        };
        ok(pat.s, row.0) && ok(pat.p, row.1) && ok(pat.o, row.2)
    }

    /// Read a row slot by position (0=S, 1=P, 2=O).
    fn slot(row: (TermId, TermId, TermId), pos: u8) -> TermId {
        match pos {
            0 => row.0,
            1 => row.1,
            _ => row.2,
        }
    }

    /// Light index-walk estimate of a single pattern's match count. Draws
    /// `SAMPLE_K` pseudo-random rows, measures the matching fraction, and scales
    /// by the row count. Returns `(estimate, standard-error band)`.
    fn sample_single(&self, pat: &TriplePattern) -> Option<(f64, f64)> {
        let n = self.sample_rows.len();
        if n == 0 {
            return Some((0.0, 0.0));
        }
        let k = SAMPLE_K.min(n);
        let mut state = Self::sample_seed(std::slice::from_ref(pat));
        let mut hits = 0u64;
        for _ in 0..k {
            let idx = (Self::lcg_next(&mut state) >> 33) as usize % n;
            if Self::row_matches(pat, self.sample_rows[idx]) {
                hits += 1;
            }
        }
        let p_hat = hits as f64 / k as f64;
        let estimate = n as f64 * p_hat;
        // Standard error of the estimated count from a proportion sample.
        let se = (p_hat * (1.0 - p_hat) / k as f64).sqrt() * n as f64;
        Some((estimate, se))
    }

    /// Light Wander-Join-style estimate for a two-pattern BGP joined on exactly
    /// one shared variable. Samples `SAMPLE_K` rows as the first pattern's row;
    /// for each match, counts the second pattern's rows sharing the join value,
    /// then scales the mean by the row count. Returns `None` for shapes it does
    /// not handle (no shared variable, or more than one). A full Wander Join is a
    /// later phase.
    fn sample_two(&self, p1: &TriplePattern, p2: &TriplePattern) -> Option<(f64, f64)> {
        let n = self.sample_rows.len();
        if n == 0 {
            return Some((0.0, 0.0));
        }
        // Find the single shared join variable and its position in each pattern.
        let vars1 = pattern_vars(p1);
        let vars2 = pattern_vars(p2);
        let shared: Vec<Var> = vars1
            .iter()
            .copied()
            .filter(|v| vars2.contains(v))
            .collect();
        if shared.len() != 1 {
            return None;
        }
        let jv = shared[0];
        let pos1 = p1.position_of(jv)?;
        let pos2 = p2.position_of(jv)?;

        let k = SAMPLE_K.min(n);
        let mut state = Self::sample_seed(&[*p1, *p2]);
        let mut sum = 0f64;
        let mut sum_sq = 0f64;
        for _ in 0..k {
            let idx = (Self::lcg_next(&mut state) >> 33) as usize % n;
            let r1 = self.sample_rows[idx];
            let contribution = if Self::row_matches(p1, r1) {
                let join_val = Self::slot(r1, pos1);
                // Count second-pattern rows matching on the join value.
                self.sample_rows
                    .iter()
                    .filter(|&&r2| Self::row_matches(p2, r2) && Self::slot(r2, pos2) == join_val)
                    .count() as f64
            } else {
                0.0
            };
            sum += contribution;
            sum_sq += contribution * contribution;
        }
        let mean = sum / k as f64;
        let estimate = n as f64 * mean;
        // Standard error of the scaled mean (Horvitz-Thompson-style band).
        let var = (sum_sq / k as f64 - mean * mean).max(0.0);
        let se = (var / k as f64).sqrt() * n as f64;
        Some((estimate, se))
    }
}

/// Distinct variables of a pattern, in S, P, O order.
fn pattern_vars(pat: &TriplePattern) -> Vec<Var> {
    let mut out = Vec::new();
    for t in [pat.s, pat.p, pat.o] {
        if let Term::Var(v) = t {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
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

    /// Tier 2: largest single-node fan-out for predicate `p` on `role`. The
    /// Subject role returns the max distinct-object count of any one subject; the
    /// Object role returns the max distinct-subject count of any one object. An
    /// unknown predicate falls back to the conservative whole-graph bound.
    fn max_degree(&self, p: TermId, role: Role) -> u64 {
        match self.max_degree.get(&p) {
            Some((subj, obj)) => match role {
                Role::Subject => *subj,
                Role::Object => *obj,
            },
            None => self.total,
        }
    }

    /// Tier-3 sampling hook. Inert by default (`sampling_enabled == false` →
    /// `None`); this is the default path and nothing in the estimator consumes
    /// it. When enabled via [`SnapshotStats::with_sampling`], returns a light,
    /// deterministic Wander-Join-style `(estimate, confidence-band)` for a
    /// single-pattern or single-join two-pattern BGP, and `None` for shapes it
    /// does not handle. This is a hook, not the production sampler — a full
    /// Wander Join is a later phase.
    fn sample_join(&self, patterns: &[TriplePattern]) -> Option<(f64, f64)> {
        if !self.sampling_enabled {
            return None;
        }
        match patterns {
            [p] => self.sample_single(p),
            [p1, p2] => self.sample_two(p1, p2),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Triple;
    use crate::pattern::{Term, Var};

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

        // Tier-1 index is now populated (Task 3): subjects 2 and 3 have the set
        // {10}, subject 1 has {10, 20}.
        let cs = stats.characteristic_sets();
        assert_eq!(cs.sets.len(), 2);
        let just_10 = cs
            .sets
            .iter()
            .find(|s| s.predicates == vec![10])
            .expect("{10} set present");
        assert_eq!(just_10.count, 2);
    }

    #[test]
    fn total_counts_deduplicated_triples() {
        // `from_triples` dedups its sorted indexes, so `total` must reflect the
        // DISTINCT triple count, not the inflated pre-dedup input length. Here the
        // input repeats (1,10,100), so 4 rows in → 3 distinct rows.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 100), // duplicate of the first
            Triple::new(2, 10, 101),
            Triple::new(3, 20, 200),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src);
        assert_eq!(stats.total_triples(), 3);
    }

    #[test]
    fn characteristic_sets_grouping() {
        // s1: predicates {10,20} — (10->100),(10->101),(20->200)
        // s2: predicates {10,20} — (10->102),(20->201)
        // s3: predicates {10}     — (10->103)
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(1, 20, 200),
            Triple::new(2, 10, 102),
            Triple::new(2, 20, 201),
            Triple::new(3, 10, 103),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src);
        let cs = stats.characteristic_sets();

        // Two distinct sets, none folded into the residual (< CS_TOP_K).
        assert_eq!(cs.sets.len(), 2);
        assert_eq!(cs.residual_subjects, 0);
        assert!(cs.residual_pred_occ.is_empty());

        let two = cs
            .sets
            .iter()
            .find(|s| s.predicates == vec![10, 20])
            .expect("{10,20} set present");
        assert_eq!(two.count, 2);
        // occurrences: pred 10 = s1(2) + s2(1) = 3; pred 20 = s1(1) + s2(1) = 2.
        assert_eq!(two.occurrences, vec![(10, 3), (20, 2)]);

        let one = cs
            .sets
            .iter()
            .find(|s| s.predicates == vec![10])
            .expect("{10} set present");
        assert_eq!(one.count, 1);
        assert_eq!(one.occurrences, vec![(10, 1)]);

        // by_predicate[10] lists BOTH sets that contain predicate 10.
        let mut idx_with_10 = cs.by_predicate.get(&10).cloned().unwrap_or_default();
        idx_with_10.sort_unstable();
        let mut expected: Vec<usize> = (0..cs.sets.len())
            .filter(|&i| cs.sets[i].predicates.contains(&10))
            .collect();
        expected.sort_unstable();
        assert_eq!(idx_with_10, expected);
        assert_eq!(idx_with_10.len(), 2);

        // by_predicate[20] lists only the {10,20} set.
        let idx_with_20 = cs.by_predicate.get(&20).cloned().unwrap_or_default();
        assert_eq!(idx_with_20.len(), 1);
        assert!(cs.sets[idx_with_20[0]].predicates.contains(&20));
    }

    #[test]
    fn characteristic_sets_residual_folding() {
        // Four distinct predicate-sets with distinct subject counts:
        //   {10}     — 4 subjects (s1..s4), 1 object each      → count 4
        //   {20}     — 3 subjects (s5..s7), 1 object each      → count 3
        //   {30}     — 2 subjects: s8 (2 objs on 30), s9 (1)   → count 2, occ(30)=3
        //   {30,40}  — 1 subject s99: 1 obj on 30, 2 objs on 40 → count 1
        // With k=2 the top two ({10},{20}) are retained; {30} and {30,40} fold.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 100),
            Triple::new(3, 10, 100),
            Triple::new(4, 10, 100),
            Triple::new(5, 20, 200),
            Triple::new(6, 20, 200),
            Triple::new(7, 20, 200),
            Triple::new(8, 30, 300),
            Triple::new(8, 30, 301),
            Triple::new(9, 30, 300),
            Triple::new(99, 30, 300),
            Triple::new(99, 40, 400),
            Triple::new(99, 40, 401),
        ];
        let src = VecTripleSource::from_triples(triples);
        let rows = src.sorted_rows(Ordering::Spo).expect("Spo ordering");
        let cs = SnapshotStats::build_characteristic_sets_with_k(rows, 2);

        // Exactly the two highest-count sets are retained, most-frequent first.
        assert_eq!(cs.sets.len(), 2);
        assert_eq!(cs.sets[0].predicates, vec![10]);
        assert_eq!(cs.sets[0].count, 4);
        assert_eq!(cs.sets[1].predicates, vec![20]);
        assert_eq!(cs.sets[1].count, 3);

        // Residual folds {30} (count 2) and {30,40} (count 1) → 3 subjects.
        assert_eq!(cs.residual_subjects, 3);
        // Per-predicate occurrences summed across folded sets, sorted by predicate:
        //   pred 30 = 3 (from {30}) + 1 (from {30,40}) = 4; pred 40 = 2.
        assert_eq!(cs.residual_pred_occ, vec![(30, 4), (40, 2)]);

        // by_predicate references only retained set indices (0..2) and only the
        // retained predicates (10, 20); folded predicates 30/40 are absent.
        let mut keys: Vec<TermId> = cs.by_predicate.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, vec![10, 20]);
        for idxs in cs.by_predicate.values() {
            for &i in idxs {
                assert!(i < cs.sets.len(), "index {i} out of retained range");
            }
        }
        assert_eq!(cs.by_predicate[&10], vec![0]);
        assert_eq!(cs.by_predicate[&20], vec![1]);
    }

    #[test]
    fn sample_join_inert_by_default() {
        // The Tier-3 sampling hook is OFF unless explicitly enabled, so
        // `sample_join` returns `None` for any BGP on a plain `SnapshotStats`.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(3, 10, 102),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src);

        // Single-pattern BGP: ?s <10> ?o
        let single = vec![TriplePattern::new(
            Term::Var(Var(0)),
            Term::Bound(10),
            Term::Var(Var(1)),
        )];
        assert!(stats.sample_join(&single).is_none());
        // Empty and multi-pattern BGPs are inert too.
        assert!(stats.sample_join(&[]).is_none());
        // Default path retains no per-graph copy.
        assert!(stats.sample_rows.is_empty());
    }

    #[test]
    fn sample_join_enabled_returns_some() {
        // p1 (=10): 3 subjects each with one object. p2 (=20): one triple.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(2, 10, 101),
            Triple::new(3, 10, 102),
            Triple::new(1, 20, 200),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src).with_sampling(&src, true);

        // Single-pattern BGP the hook handles: ?s <10> ?o.
        let single = vec![TriplePattern::new(
            Term::Var(Var(0)),
            Term::Bound(10),
            Term::Var(Var(1)),
        )];
        let first = stats.sample_join(&single).expect("hook enabled → Some");
        let (est, ci) = first;
        assert!(
            est >= 0.0 && est.is_finite(),
            "estimate {est} finite & >= 0"
        );
        assert!(ci >= 0.0 && ci.is_finite(), "confidence {ci} finite & >= 0");

        // Deterministic: a second call yields exactly the same value.
        let second = stats.sample_join(&single).expect("hook enabled → Some");
        assert_eq!(first, second);
    }

    #[test]
    fn max_degree_basic() {
        // Same base graph, plus a shared object 900 on pred 30 from s1 and s2.
        let triples = vec![
            Triple::new(1, 10, 100),
            Triple::new(1, 10, 101),
            Triple::new(1, 20, 200),
            Triple::new(2, 10, 102),
            Triple::new(2, 20, 201),
            Triple::new(3, 10, 103),
            Triple::new(1, 30, 900),
            Triple::new(2, 30, 900),
        ];
        let src = VecTripleSource::from_triples(triples);
        let stats = SnapshotStats::from_source(&src);

        // Subject role = object fan-out per subject: s1 has {100,101} on pred 10.
        assert_eq!(stats.max_degree(10, Role::Subject), 2);
        // Object role = subject fan-out per object: object 900 on pred 30 has {s1,s2}.
        assert_eq!(stats.max_degree(30, Role::Object), 2);
        // Object 900's subject fan-out (2) dominates the subject fan-out on pred 30
        // (each subject has one object) — sanity-check the roles are not swapped.
        assert_eq!(stats.max_degree(30, Role::Subject), 1);
        // Unknown predicate falls back to the conservative whole-graph bound.
        assert_eq!(stats.max_degree(999, Role::Subject), stats.total_triples());
    }
}
