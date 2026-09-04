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
use crate::plan::{degree_order, JoinSpec};
use crate::stats::{Position, Stats};

/// Cost of hashing one row on a hash join's build side, in row-reads.
/// ponytail: uncalibrated knob. The unified-memory materialisation term of
/// SPEC-23 §5.5 — tune on `hornbench` once a hybrid plan is ever chosen.
pub const HASH_BUILD_WEIGHT: f64 = 8.0;
/// Cost of writing one row into an intermediate buffer. The tree evaluator
/// (`executor/binary_hash.rs`) holds every node as `Vec<Vec<TermId>>` — one
/// heap row per binding — so this is an order of magnitude above a leapfrog
/// seek, not one. The whole-BGP leapfrog streams into batches and pays none.
pub const MATERIALIZE_WEIGHT: f64 = 8.0;
/// A hybrid plan must be estimated at least this many times cheaper than
/// the whole-BGP leapfrog to replace it: the model's error bars are wider
/// than any small win, and the leapfrog is the production-proven path.
pub const HYBRID_MARGIN: f64 = 2.0;
/// Coarse prior for an unbound-predicate pattern with one endpoint fixed:
/// the run is `total / this`. Matches the estimator's static prior.
const UNBOUND_PRED_DIVISOR: f64 = 25.0;
/// AGM enumeration is `3^k` over the node's patterns; past this many the
/// bound is skipped (no cap) rather than paid for. Every DP subset asks, so
/// `sum over subsets of 3^|S|` is the planning-time driver — keep it small.
const AGM_MAX_PATTERNS: usize = 5;

/// A bitmask of pattern indices into `Bgp::patterns`. Planning past 64
/// patterns falls back to structural routing (see `planner.rs`).
pub type Mask = u64;

pub struct CostModel<'a, S: Stats + ?Sized> {
    bgp: &'a Bgp,
    stats: &'a S,
    est: StatsEstimator<'a, S>,
    card_memo: RefCell<HashMap<Mask, f64>>,
    wcoj_memo: RefCell<HashMap<Mask, WcojCost>>,
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
            wcoj_memo: RefCell::new(HashMap::new()),
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

    /// Estimated output rows of the sub-BGP `mask` (the planning-grade
    /// [`StatsEstimator::estimate_bgp_fast`]), capped by its AGM bound.
    pub fn card(&self, mask: Mask) -> f64 {
        if let Some(c) = self.card_memo.borrow().get(&mask) {
            return *c;
        }
        let pats = self.patterns_of(mask);
        let est = self.est.estimate_bgp_fast(&pats).estimate as f64;
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

    /// Cost of a WCOJ node over `mask`: its i-cost, plus materialising its
    /// rows when it is a sub-node of a hybrid plan (the whole-BGP node
    /// streams and pays nothing).
    pub fn wcoj_node_cost(&self, mask: Mask, whole: Mask) -> f64 {
        let patterns: Vec<usize> = (0..self.bgp.patterns.len())
            .filter(|i| mask >> i & 1 == 1)
            .collect();
        let c = self.wcoj(&patterns).cost;
        if mask == whole {
            c
        } else {
            c + self.card(mask) * MATERIALIZE_WEIGHT
        }
    }

    /// Estimated cost of a whole plan on the model's scale; `whole` is the
    /// mask of every non-ground pattern.
    pub fn cost_of(&self, spec: &JoinSpec, whole: Mask) -> f64 {
        match spec {
            JoinSpec::Scan { pattern } => self.scan_cost(*pattern),
            JoinSpec::Wcoj { patterns, .. } => {
                let mask = patterns.iter().fold(0 as Mask, |m, &i| m | (1 << i));
                self.wcoj_node_cost(mask, whole)
            }
            JoinSpec::HashJoin { build, probe } => {
                let mask_of =
                    |s: &JoinSpec| s.patterns().iter().fold(0 as Mask, |m, &i| m | (1 << i));
                self.cost_of(build, whole)
                    + self.cost_of(probe, whole)
                    + self.join_cost(mask_of(build), mask_of(probe))
            }
        }
    }

    /// Elimination order and i-cost for one WCOJ node over `patterns`.
    ///
    /// The order is the structural degree order, constrained to stay
    /// connected: after the first variable, only variables sharing a pattern
    /// with the bound prefix are candidates (a disconnected pick is a cross
    /// product at that depth). Among candidates: highest degree first, then
    /// the smaller estimated intersection — compared by decimal order of
    /// magnitude only, the granularity the estimator is accurate to (SPEC-23
    /// §7.3) — then the cheaper depth, then first appearance.
    ///
    /// For the whole-BGP node the first variable is also tried as each of the
    /// two most selective variables (fewest rows when bound first, e.g. a
    /// `?customer` filtered by a bound country), and the cheapest complete
    /// order wins — the HDB-108 win a degree-first start cannot see. Sub-nodes
    /// of a hybrid skip the sweep. Uninformed statistics collapse everything
    /// to the plain degree order.
    pub fn wcoj(&self, patterns: &[usize]) -> WcojCost {
        let mask = patterns.iter().fold(0 as Mask, |m, &i| m | (1 << i));
        if let Some(c) = self.wcoj_memo.borrow().get(&mask) {
            return c.clone();
        }
        let c = self.wcoj_uncached(patterns);
        self.wcoj_memo.borrow_mut().insert(mask, c.clone());
        c
    }

    fn wcoj_uncached(&self, patterns: &[usize]) -> WcojCost {
        let sub = Bgp::new(patterns.iter().map(|&i| self.bgp.patterns[i]).collect());
        let vars = sub.variables();
        if !self.informed() || vars.len() <= 1 {
            return WcojCost {
                var_order: degree_order(&sub),
                cost: f64::INFINITY,
            };
        }
        let search = OrderSearch::new(self, &sub, vars);
        let mut best = search.run(None);
        let live = self.bgp.patterns.iter().filter(|p| !p.is_ground()).count();
        if patterns.len() == live {
            let mut starts: Vec<(i32, usize)> = (0..search.vars.len())
                .map(|i| (bucket(search.extend(i, &[], 1.0).0), i))
                .collect();
            starts.sort_unstable();
            for &(_, i) in starts.iter().take(2) {
                if search.vars[i] == best.var_order[0] {
                    continue;
                }
                let c = search.run(Some(i));
                if c.cost < best.cost {
                    best = c;
                }
            }
        }
        best
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

/// Decimal order of magnitude of a row count — the granularity two
/// estimates are compared at.
fn bucket(rows: f64) -> i32 {
    rows.max(1.0).log10().floor() as i32
}

/// One node's elimination-order search state: its variables (first
/// appearance order), which patterns mention each, and each pattern's
/// variables — built once per node so every step is an index walk.
struct OrderSearch<'m, 'a, S: Stats + ?Sized> {
    model: &'m CostModel<'a, S>,
    sub: &'m Bgp,
    vars: Vec<Var>,
    by_var: Vec<Vec<usize>>,
    pat_vars: Vec<Vec<Var>>,
}

impl<'m, 'a, S: Stats + ?Sized> OrderSearch<'m, 'a, S> {
    fn new(model: &'m CostModel<'a, S>, sub: &'m Bgp, vars: Vec<Var>) -> Self {
        let pat_vars: Vec<Vec<Var>> = sub.patterns.iter().map(pattern_vars).collect();
        let by_var = vars
            .iter()
            .map(|v| {
                (0..sub.patterns.len())
                    .filter(|&p| pat_vars[p].contains(v))
                    .collect()
            })
            .collect();
        Self {
            model,
            sub,
            vars,
            by_var,
            pat_vars,
        }
    }

    /// Connected degree-first order, optionally forced to start at `first`.
    fn run(&self, first: Option<usize>) -> WcojCost {
        let n = self.vars.len();
        let mut order: Vec<Var> = Vec::with_capacity(n);
        let mut done = vec![false; n];
        let mut rows = 1.0f64;
        let mut cost = 0.0f64;
        if let Some(i) = first {
            let (out, step) = self.extend(i, &order, rows);
            order.push(self.vars[i]);
            done[i] = true;
            cost += step;
            rows = out.max(1.0);
        }
        while order.len() < n {
            let touches = |i: usize| {
                self.by_var[i]
                    .iter()
                    .any(|&p| self.pat_vars[p].iter().any(|u| order.contains(u)))
            };
            let unbound = || (0..n).filter(|&i| !done[i]);
            let mut candidates: Vec<usize> = unbound().filter(|&i| touches(i)).collect();
            if candidates.is_empty() {
                candidates = unbound().collect();
            }
            let mut best: Option<(std::cmp::Reverse<usize>, i32, f64)> = None;
            let mut pick = (0.0f64, 0.0f64, candidates[0]);
            for i in candidates {
                let (out, step) = self.extend(i, &order, rows);
                let key = (std::cmp::Reverse(self.by_var[i].len()), bucket(out), step);
                // Strict `<`: first appearance wins ties.
                if best.is_none_or(|b| key < b) {
                    best = Some(key);
                    pick = (out, step, i);
                }
            }
            let (out, step, i) = pick;
            order.push(self.vars[i]);
            done[i] = true;
            cost += step;
            rows = out.max(1.0);
        }
        WcojCost {
            var_order: order,
            cost,
        }
    }

    /// Extend a prefix of `rows` bindings over `prefix` by variable index
    /// `i`: returns `(rows after, i-cost of this depth)`.
    fn extend(&self, i: usize, prefix: &[Var], rows: f64) -> (f64, f64) {
        let v = self.vars[i];
        let mut runs: Vec<f64> = Vec::with_capacity(self.by_var[i].len());
        let mut dom = 1.0f64;
        for &p in &self.by_var[i] {
            let p = &self.sub.patterns[p];
            runs.push(self.model.run(p, v, prefix));
            dom = dom.max(self.model.domain(p, v));
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
