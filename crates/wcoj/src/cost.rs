//! Cost model for per-BGP join planning (SPEC-23 §5.5, PLAN-23-04).
//!
//! One additive scale — "rows touched" — so a leapfrog (WCOJ) step and a
//! hash-join step compare on equal footing:
//!
//! - A WCOJ node pays **i-cost** per elimination depth: for every prefix
//!   binding, the contributing iterators intersect their candidate runs.
//!   The smallest run is scanned and the others are galloped, so a depth
//!   costs `prefix_rows * k * min_run * (1 + log2(max_run / min_run))`, plus
//!   one `open_level` per iterator per prefix row. Output rows stream into
//!   batches at [`MATERIALIZE_WEIGHT`] each.
//! - A hash join pays [`HASH_BUILD_WEIGHT`] per row hashed on the smaller
//!   side, one per row probed, and [`MATERIALIZE_WEIGHT`] per output row. A
//!   scan leaf pays one read plus one materialise per matching triple.
//!
//! Cardinalities come from the Phase-3 [`StatsEstimator`], capped by the AGM
//! (fractional-edge-cover) bound of the sub-BGP — the sound upper bound the
//! denominator model can overshoot on cyclic shapes. The structural
//! decomposition ([`CostModel::cyclic_core`]) is the GYO ear-removal test:
//! whatever it cannot reduce is the cyclic core Freitag routes to WCOJ.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::estimator::StatsEstimator;
use crate::pattern::{Bgp, Term, TriplePattern, Var};
use crate::plan::degree_order;
use crate::stats::{Position, Stats};

/// Cost of hashing one row on a hash join's build side, in row-reads.
/// ponytail: uncalibrated knob. The unified-memory materialisation term of
/// SPEC-23 §5.5 — tune on `hornbench` once a hybrid plan is ever chosen.
pub const HASH_BUILD_WEIGHT: f64 = 4.0;
/// Cost of writing one row into an intermediate or output buffer.
pub const MATERIALIZE_WEIGHT: f64 = 1.0;
/// Coarse prior for an unbound-predicate pattern with one endpoint fixed:
/// the run is `total / this`. Matches the estimator's static prior.
const UNBOUND_PRED_DIVISOR: f64 = 25.0;
/// AGM enumeration is `3^k` over the node's patterns; past this many the
/// bound is skipped (no cap) rather than paid for.
const AGM_MAX_PATTERNS: usize = 10;

/// A bitmask of pattern indices into `Bgp::patterns`. Planning past 64
/// patterns falls back to structural routing (see `planner.rs`).
pub type Mask = u64;

pub struct CostModel<'a, S: Stats + ?Sized> {
    bgp: &'a Bgp,
    stats: &'a S,
    est: StatsEstimator<'a, S>,
    card_memo: RefCell<HashMap<Mask, f64>>,
}

/// A costed WCOJ node: its elimination order and the i-cost of running it.
#[derive(Debug, Clone, PartialEq)]
pub struct WcojCost {
    pub var_order: Vec<Var>,
    pub cost: f64,
}

impl<'a, S: Stats + ?Sized> CostModel<'a, S> {
    pub fn new(bgp: &'a Bgp, stats: &'a S) -> Self {
        Self {
            bgp,
            stats,
            est: StatsEstimator::new(stats),
            card_memo: RefCell::new(HashMap::new()),
        }
    }

    pub fn bgp(&self) -> &'a Bgp {
        self.bgp
    }

    /// Whether the statistics carry per-predicate signal. Without it the
    /// planner routes structurally instead of by cost.
    pub fn informed(&self) -> bool {
        self.stats.is_informed()
    }

    fn patterns_of(&self, mask: Mask) -> Vec<TriplePattern> {
        (0..self.bgp.patterns.len())
            .filter(|i| mask >> i & 1 == 1)
            .map(|i| self.bgp.patterns[i])
            .collect()
    }

    /// Estimated output rows of the sub-BGP `mask`, capped by its AGM bound.
    pub fn card(&self, mask: Mask) -> f64 {
        if let Some(c) = self.card_memo.borrow().get(&mask) {
            return *c;
        }
        let pats = self.patterns_of(mask);
        let est = self.est.estimate_bgp(&pats).estimate as f64;
        let c = est.min(self.agm_bound(mask)).max(0.0);
        self.card_memo.borrow_mut().insert(mask, c);
        c
    }

    /// AGM bound of the sub-BGP: `min over edge covers x of prod |R_e|^x_e`,
    /// with `|R_e|` each pattern's sound per-pattern maximum. Enumerates
    /// half-integral covers (`x_e in {0, 1/2, 1}`) — optimal for paths,
    /// stars and cycles, and any feasible cover is still an upper bound.
    /// `f64::INFINITY` when there is nothing to bound.
    pub fn agm_bound(&self, mask: Mask) -> f64 {
        let edges: Vec<(Vec<Var>, f64)> = (0..self.bgp.patterns.len())
            .filter(|i| mask >> i & 1 == 1)
            .map(|i| &self.bgp.patterns[i])
            .filter(|p| !p.is_ground())
            .map(|p| {
                let size = self.est.estimate_pattern(p).upper_bound as f64;
                (pattern_vars(p), size)
            })
            .collect();
        let k = edges.len();
        if k == 0 || k > AGM_MAX_PATTERNS {
            return f64::INFINITY;
        }
        let mut vars: Vec<Var> = Vec::new();
        for (vs, _) in &edges {
            for v in vs {
                if !vars.contains(v) {
                    vars.push(*v);
                }
            }
        }
        let mut best = f64::INFINITY;
        let mut x = vec![0u8; k]; // 0, 1, 2 -> 0, 1/2, 1
        loop {
            let covered = vars.iter().all(|v| {
                let total: u32 = edges
                    .iter()
                    .zip(&x)
                    .filter(|((vs, _), _)| vs.contains(v))
                    .map(|(_, &xe)| xe as u32)
                    .sum();
                total >= 2
            });
            if covered {
                let bound = edges
                    .iter()
                    .zip(&x)
                    .map(|((_, size), &xe)| size.powf(xe as f64 / 2.0))
                    .product::<f64>();
                best = best.min(bound);
            }
            // Next base-3 counter value.
            let mut i = 0;
            loop {
                if i == k {
                    return best;
                }
                if x[i] < 2 {
                    x[i] += 1;
                    break;
                }
                x[i] = 0;
                i += 1;
            }
        }
    }

    /// GYO ear removal over the non-ground patterns in `live`: repeatedly
    /// drop a variable mentioned by only one pattern, then any pattern whose
    /// remaining variables sit inside another's. What survives is the cyclic
    /// core (empty for an acyclic BGP), as original pattern indices.
    pub fn cyclic_core(&self, live: &[usize]) -> Vec<usize> {
        let mut edges: Vec<(usize, Vec<Var>)> = live
            .iter()
            .map(|&i| (i, pattern_vars(&self.bgp.patterns[i])))
            .filter(|(_, vs)| !vs.is_empty())
            .collect();
        loop {
            let mut changed = false;
            // (a) ears: a variable in exactly one edge carries no join.
            let mut counts: HashMap<Var, usize> = HashMap::new();
            for (_, vs) in &edges {
                for v in vs {
                    *counts.entry(*v).or_default() += 1;
                }
            }
            for (_, vs) in edges.iter_mut() {
                let before = vs.len();
                vs.retain(|v| counts[v] > 1);
                changed |= vs.len() != before;
            }
            // (b) an edge inside another edge is absorbed by it.
            let n = edges.len();
            let mut drop: Option<usize> = None;
            'outer: for i in 0..n {
                if edges[i].1.is_empty() {
                    drop = Some(i);
                    break;
                }
                for j in 0..n {
                    if i != j && edges[i].1.iter().all(|v| edges[j].1.contains(v)) {
                        drop = Some(i);
                        break 'outer;
                    }
                }
            }
            if let Some(i) = drop {
                edges.remove(i);
                changed = true;
            }
            if !changed {
                break;
            }
        }
        let mut core: Vec<usize> = edges.into_iter().map(|(i, _)| i).collect();
        core.sort_unstable();
        core
    }

    /// Cost of materialising one pattern's matches.
    pub fn scan_cost(&self, pattern: usize) -> f64 {
        self.card(1 << pattern) * (1.0 + MATERIALIZE_WEIGHT)
    }

    /// Cost of hash-joining two costed subplans (build side = the smaller).
    pub fn join_cost(&self, left: Mask, right: Mask) -> f64 {
        let (a, b) = (self.card(left), self.card(right));
        let out = self.card(left | right);
        a.min(b) * HASH_BUILD_WEIGHT + a.max(b) + out * MATERIALIZE_WEIGHT
    }

    /// Greedy elimination order and i-cost for one WCOJ node over `patterns`:
    /// at each depth pick the variable with the smallest estimated
    /// intersection (fewest rows after extending), ties by that depth's
    /// i-cost, then descending degree, then first appearance. Uninformed
    /// statistics collapse this to the structural degree order.
    pub fn wcoj(&self, patterns: &[usize]) -> WcojCost {
        let sub = Bgp::new(patterns.iter().map(|&i| self.bgp.patterns[i]).collect());
        let all_vars = sub.variables();
        let mask = patterns.iter().fold(0 as Mask, |m, &i| m | (1 << i));
        if !self.informed() || all_vars.len() <= 1 {
            return WcojCost {
                var_order: degree_order(&sub),
                cost: f64::INFINITY,
            };
        }
        let degree_rank = degree_order(&sub);
        let mut order: Vec<Var> = Vec::with_capacity(all_vars.len());
        let mut rows = 1.0f64;
        let mut cost = 0.0f64;
        while order.len() < all_vars.len() {
            let mut best: Option<(f64, f64, usize, Var)> = None;
            for &v in &all_vars {
                if order.contains(&v) {
                    continue;
                }
                let (out, step) = self.extend(&sub, v, &order, rows);
                let rank = degree_rank
                    .iter()
                    .position(|d| *d == v)
                    .unwrap_or(usize::MAX);
                let key = (out, step, rank, v);
                let better = match best {
                    None => true,
                    Some((bo, bs, br, _)) => (out, step, rank) < (bo, bs, br),
                };
                if better {
                    best = Some(key);
                }
            }
            let (out, step, _, v) = best.expect("unordered variable remains");
            order.push(v);
            cost += step;
            rows = out.max(1.0);
        }
        // Final output row count: the whole node's estimate, not the model's
        // running product, keeps the two candidates on one scale.
        cost += self.card(mask) * MATERIALIZE_WEIGHT;
        WcojCost {
            var_order: order,
            cost,
        }
    }

    /// Extend a prefix of `rows` bindings over `prefix` by variable `v`:
    /// returns `(rows after, i-cost of this depth)`.
    fn extend(&self, sub: &Bgp, v: Var, prefix: &[Var], rows: f64) -> (f64, f64) {
        let mut runs: Vec<f64> = Vec::new();
        let mut dom = 1.0f64;
        for p in &sub.patterns {
            if p.position_of(v).is_none() {
                continue;
            }
            runs.push(self.run(p, v, prefix));
            dom = dom.max(self.domain(p, v));
        }
        let k = runs.len() as f64;
        let min_run = runs.iter().cloned().fold(f64::INFINITY, f64::min).max(1.0);
        let max_run = runs.iter().cloned().fold(0.0, f64::max).max(1.0);
        // Intersection size: the smallest run, thinned by every other run's
        // selectivity against the variable's domain.
        let min_at = runs.iter().position(|r| *r == min_run).unwrap_or(0);
        let mut inter = min_run;
        for (i, r) in runs.iter().enumerate() {
            if i != min_at {
                inter *= (r / dom).min(1.0);
            }
        }
        let step = rows * (k + k * min_run * (1.0 + (max_run / min_run).log2()));
        (rows * inter, step)
    }

    /// Candidate run of pattern `p` for variable `v` given the bound prefix:
    /// a fan-out list when the other endpoint is fixed, else the position's
    /// whole domain.
    fn run(&self, p: &TriplePattern, v: Var, prefix: &[Var]) -> f64 {
        let fixed = |t: Term| match t {
            Term::Bound(_) => true,
            Term::Var(u) => prefix.contains(&u),
        };
        let pos = p.position_of(v).expect("variable not in pattern");
        let total = self.stats.total_triples() as f64;
        match p.p {
            Term::Bound(pid) => {
                let count = self.stats.predicate_count(pid) as f64;
                let (other, other_pos) = if pos == 0 {
                    (p.o, Position::Object)
                } else {
                    (p.s, Position::Subject)
                };
                if fixed(other) {
                    (count / self.stats.ndv(pid, other_pos).max(1) as f64).max(1.0)
                } else {
                    self.domain(p, v)
                }
            }
            Term::Var(_) if pos == 1 => (self.stats.distinct_predicates() as f64).max(1.0),
            Term::Var(_) => {
                let other = if pos == 0 { p.o } else { p.s };
                if fixed(other) {
                    (total / UNBOUND_PRED_DIVISOR).max(1.0)
                } else {
                    total.max(1.0)
                }
            }
        }
    }

    /// Number of distinct values `v`'s position in `p` can take.
    fn domain(&self, p: &TriplePattern, v: Var) -> f64 {
        let pos = p.position_of(v).expect("variable not in pattern");
        let d = match (p.p, pos) {
            (Term::Bound(pid), 0) => self.stats.ndv(pid, Position::Subject),
            (Term::Bound(pid), _) => self.stats.ndv(pid, Position::Object),
            (Term::Var(_), 1) => self.stats.distinct_predicates(),
            (Term::Var(_), _) => self.stats.total_triples(),
        };
        (d as f64).max(1.0)
    }
}

/// Distinct variables of a pattern in S/P/O order.
pub fn pattern_vars(p: &TriplePattern) -> Vec<Var> {
    let mut out = Vec::new();
    for t in [p.s, p.p, p.o] {
        if let Some(v) = t.as_var() {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}
