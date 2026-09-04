//! Cost-based per-BGP join planning (SPEC-23 §5.5, PLAN-23-04).
//!
//! [`Planner::choose`] turns a BGP plus the Phase-3 [`Stats`] seam into a
//! [`JoinSpec`] in three layers:
//!
//! 1. **Structural routing** (Freitag, VLDB 2020): GYO ear removal finds the
//!    cyclic core. The core is never split by a hash join — it is always one
//!    WCOJ node — because a binary plan on a cycle materialises the large
//!    intermediate the leapfrog avoids.
//! 2. **Cost** ([`crate::cost`]): i-cost for WCOJ nodes, build+probe for hash
//!    joins, one additive scale.
//! 3. **Search**: dynamic programming over connected pattern subsets, each
//!    realised either as one WCOJ node or as a hash join of two smaller
//!    subsets (DPccp restricted to core-respecting subsets). Past
//!    [`MAX_DP_PATTERNS`] a greedy build-up over units (each cyclic core,
//!    every other pattern) runs instead. Hash build sides are assigned in a
//!    late pass so the DP state stays symmetric. Whatever the search finds must beat the
//!    whole-BGP leapfrog by [`crate::cost::HYBRID_MARGIN`], or the leapfrog
//!    runs.
//!
//! Statistics with no per-predicate signal (`Stats::is_informed() == false`)
//! skip the cost search: the whole BGP goes to one WCOJ node in structural
//! degree order, the production path. `HORNDB_WCOJ_CUTOVER=<n>` restores
//! the retired Stage-1 pattern-count rule for bisection.

use std::sync::OnceLock;

use crate::cost::{CostModel, Mask, HYBRID_MARGIN};
use crate::pattern::Bgp;
use crate::plan::{degree_order, ExecutionPlan, JoinSpec};
use crate::stats::Stats;

/// Past this many non-ground patterns the DP is skipped for the greedy. The
/// DP costs every connected subset (up to `2^k`, each an estimate plus an
/// elimination-order search of a few microseconds), so this is the whole
/// planning-time guard: 5 keeps a dense BGP under ~200 µs in release.
pub const MAX_DP_PATTERNS: usize = 5;

#[derive(Debug, Clone)]
pub struct Planner {
    pub max_dp_patterns: usize,
    /// `Some(n)`: the retired fixed cutover (`>= n` patterns → WCOJ).
    pub fixed_cutover: Option<usize>,
}

impl Default for Planner {
    fn default() -> Self {
        static CUTOVER: OnceLock<Option<usize>> = OnceLock::new();
        let fixed_cutover = *CUTOVER.get_or_init(|| {
            std::env::var("HORNDB_WCOJ_CUTOVER")
                .ok()
                .and_then(|v| v.trim().parse().ok())
        });
        Self {
            max_dp_patterns: MAX_DP_PATTERNS,
            fixed_cutover,
        }
    }
}

impl Planner {
    pub fn choose(&self, bgp: &Bgp, stats: &dyn Stats) -> JoinSpec {
        if let Some(cutover) = self.fixed_cutover {
            return JoinSpec::from_execution_plan(&ExecutionPlan::for_bgp(bgp, cutover), bgp);
        }
        let n = bgp.patterns.len();
        let live: Vec<usize> = (0..n).filter(|&i| !bgp.patterns[i].is_ground()).collect();
        let ground: Vec<usize> = (0..n).filter(|&i| bgp.patterns[i].is_ground()).collect();
        if live.is_empty() {
            // Fully ground (or empty): membership tests only, the binary
            // path's job. An empty BGP is the join identity — one empty row.
            return JoinSpec::from_execution_plan(&ExecutionPlan::for_bgp(bgp, usize::MAX), bgp);
        }
        let model = CostModel::new(bgp, stats);
        // A single pattern has no join to plan: its order is structural, so
        // the plan never depends on whether statistics are ready yet (row
        // order, and with it ORDER BY tie-breaking, stays put across runs).
        if !model.informed() || n > Mask::BITS as usize || live.len() == 1 {
            return JoinSpec::Wcoj {
                patterns: (0..n).collect(),
                var_order: degree_order(bgp),
            };
        }
        let full: Mask = live.iter().fold(0, |m, &i| m | (1 << i));
        let adj = adjacency(bgp, &live);
        let core: Mask = model
            .cyclic_core(&live)
            .iter()
            .fold(0, |m, &i| m | (1 << i));
        let search = Search {
            model: &model,
            live: &live,
            full,
            cores: components(core, &adj),
            adj,
        };
        let found = if live.len() <= self.max_dp_patterns {
            search.dp()
        } else {
            None
        }
        .or_else(|| search.greedy());
        let mut spec = match found {
            Some(hybrid)
                if model.cost_of(&hybrid, full) * HYBRID_MARGIN
                    < model.wcoj_node_cost(full, full) =>
            {
                hybrid
            }
            _ => return whole_wcoj(&model, &live, &ground),
        };
        assign_build_sides(&model, &mut spec);
        attach_ground(&model, spec, &live, &ground)
    }
}

/// Connected components of `mask` under `adj`, as masks.
fn components(mask: Mask, adj: &[Mask]) -> Vec<Mask> {
    let mut out = Vec::new();
    let mut rest = mask;
    while rest != 0 {
        let mut seen: Mask = 1 << rest.trailing_zeros();
        loop {
            let mut next = seen;
            for (i, a) in adj.iter().enumerate() {
                if seen >> i & 1 == 1 {
                    next |= a & mask;
                }
            }
            if next == seen {
                break;
            }
            seen = next;
        }
        out.push(seen);
        rest &= !seen;
    }
    out
}

/// One WCOJ node over every pattern (ground ones included — the leapfrog
/// executor pre-checks them).
fn whole_wcoj<S: Stats + ?Sized>(
    model: &CostModel<'_, S>,
    live: &[usize],
    ground: &[usize],
) -> JoinSpec {
    let mut patterns: Vec<usize> = live.iter().chain(ground).copied().collect();
    patterns.sort_unstable();
    JoinSpec::Wcoj {
        var_order: model.wcoj(live).var_order,
        patterns,
    }
}

/// Fold ground patterns into the plan: into the WCOJ node when the plan is
/// one, else as build-side scans at the root (an absent ground triple
/// empties the join before the probe side runs).
fn attach_ground<S: Stats + ?Sized>(
    model: &CostModel<'_, S>,
    spec: JoinSpec,
    live: &[usize],
    ground: &[usize],
) -> JoinSpec {
    if ground.is_empty() {
        return spec;
    }
    if let JoinSpec::Wcoj { patterns, .. } = &spec {
        if patterns.len() == live.len() {
            return whole_wcoj(model, live, ground);
        }
    }
    ground
        .iter()
        .fold(spec, |probe, &pattern| JoinSpec::HashJoin {
            build: Box::new(JoinSpec::Scan { pattern }),
            probe: Box::new(probe),
        })
}

/// `adj[i]` = mask of live patterns sharing a variable with pattern `i`.
fn adjacency(bgp: &Bgp, live: &[usize]) -> Vec<Mask> {
    let mut adj = vec![0 as Mask; bgp.patterns.len()];
    for &i in live {
        for &j in live {
            if i == j {
                continue;
            }
            let shares = crate::cost::pattern_vars(&bgp.patterns[i])
                .iter()
                .any(|v| bgp.patterns[j].position_of(*v).is_some());
            if shares {
                adj[i] |= 1 << j;
            }
        }
    }
    adj
}

struct Search<'m, 'a, S: Stats + ?Sized> {
    model: &'m CostModel<'a, S>,
    live: &'m [usize],
    full: Mask,
    /// One mask per cyclic core (a BGP can hold several, e.g. two
    /// vertex-disjoint triangles); no hash join ever splits one.
    cores: Vec<Mask>,
    adj: Vec<Mask>,
}

impl<S: Stats + ?Sized> Search<'_, '_, S> {
    fn members(&self, mask: Mask) -> Vec<usize> {
        self.live
            .iter()
            .copied()
            .filter(|&i| mask >> i & 1 == 1)
            .collect()
    }

    fn neighbours(&self, mask: Mask) -> Mask {
        self.members(mask).iter().fold(0, |m, &i| m | self.adj[i]) & !mask
    }

    fn connected(&self, mask: Mask) -> bool {
        if mask == 0 {
            return false;
        }
        let start = 1 << mask.trailing_zeros();
        let mut seen = start;
        loop {
            let next = seen | (self.neighbours(seen) & mask);
            if next == seen {
                return seen == mask;
            }
            seen = next;
        }
    }

    /// Freitag's rule: a subset either holds a whole cyclic core or none of
    /// it, so no hash join ever splits a core.
    fn respects_cores(&self, mask: Mask) -> bool {
        self.cores.iter().all(|&c| mask & c == 0 || mask & c == c)
    }

    fn wcoj_node(&self, mask: Mask) -> (f64, JoinSpec) {
        let patterns = self.members(mask);
        let c = self.model.wcoj(&patterns);
        (
            self.model.wcoj_node_cost(mask, self.full),
            JoinSpec::Wcoj {
                patterns,
                var_order: c.var_order,
            },
        )
    }

    /// The cheapest single-node realisation of `mask`: a scan for one
    /// pattern when that is cheaper than a one-pattern leapfrog.
    fn unit(&self, mask: Mask) -> (f64, JoinSpec) {
        let wcoj = self.wcoj_node(mask);
        if mask.count_ones() == 1 {
            let p = mask.trailing_zeros() as usize;
            let scan = (self.model.scan_cost(p), JoinSpec::Scan { pattern: p });
            if scan.0 < wcoj.0 {
                return scan;
            }
        }
        wcoj
    }

    /// DP over connected, core-respecting subsets. `None` when the BGP is
    /// disconnected (a cross product the leapfrog handles natively).
    fn dp(&self) -> Option<JoinSpec> {
        let full = self.full;
        let mut best: std::collections::HashMap<Mask, (f64, JoinSpec)> =
            std::collections::HashMap::new();
        // Enumerate submasks of `full` in increasing popcount so every
        // split's halves are already solved.
        let mut masks: Vec<Mask> = submasks(full).collect();
        masks.sort_by_key(|m| m.count_ones());
        for mask in masks {
            if mask == 0 || !self.connected(mask) || !self.respects_cores(mask) {
                continue;
            }
            let (mut cost, mut spec) = self.unit(mask);
            // Every split into two solved halves that share a variable.
            let rest = mask & (mask - 1); // drop the lowest bit
            for a in submasks(rest) {
                let a = a | (mask & !rest); // lowest bit always in `a`: dedups (a, b)
                let b = mask & !a;
                if b == 0 {
                    continue;
                }
                let (Some((ca, _)), Some((cb, _))) = (best.get(&a), best.get(&b)) else {
                    continue;
                };
                if self.neighbours(a) & b == 0 {
                    continue;
                }
                let c = ca + cb + self.model.join_cost(a, b);
                if c < cost {
                    cost = c;
                    spec = JoinSpec::HashJoin {
                        build: Box::new(best[&a].1.clone()),
                        probe: Box::new(best[&b].1.clone()),
                    };
                }
            }
            best.insert(mask, (cost, spec));
        }
        best.remove(&full).map(|(_, s)| s)
    }

    /// Greedy build-up over units (each cyclic core as one WCOJ node, every
    /// other pattern on its own): seed with the cheapest unit, then
    /// repeatedly hash-join the most selective connected unit (fewest
    /// estimated rows) — one estimate per unit, so a wide BGP plans in
    /// microseconds; the margin check in `choose` costs the result once.
    /// `None` only when there is a single unit (nothing to join).
    fn greedy(&self) -> Option<JoinSpec> {
        let mut units: Vec<Mask> = self.cores.clone();
        for &p in self.live {
            if !self.cores.iter().any(|c| c >> p & 1 == 1) {
                units.push(1 << p);
            }
        }
        if units.len() < 2 {
            return None;
        }
        let cheapest = |cands: &[Mask], key: &dyn Fn(Mask) -> f64| -> Mask {
            *cands
                .iter()
                .min_by(|&&a, &&b| {
                    key(a)
                        .partial_cmp(&key(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("non-empty candidates")
        };
        let seed = cheapest(&units, &|u| self.unit(u).0);
        let (_, mut spec) = self.unit(seed);
        let mut mask = seed;
        units.retain(|&u| u != seed);
        while !units.is_empty() {
            let connected: Vec<Mask> = units
                .iter()
                .copied()
                .filter(|&u| self.neighbours(mask) & u != 0)
                .collect();
            let cands = if connected.is_empty() {
                units.clone()
            } else {
                connected
            };
            let u = cheapest(&cands, &|u| self.model.card(u));
            spec = JoinSpec::HashJoin {
                build: Box::new(self.unit(u).1),
                probe: Box::new(spec),
            };
            mask |= u;
            units.retain(|&x| x != u);
        }
        Some(spec)
    }
}

/// All submasks of `mask`, including 0 and `mask` itself.
fn submasks(mask: Mask) -> impl Iterator<Item = Mask> {
    let mut cur: Option<Mask> = Some(mask);
    std::iter::from_fn(move || {
        let m = cur?;
        cur = if m == 0 { None } else { Some((m - 1) & mask) };
        Some(m)
    })
}

/// Late pass (DuckDB `BuildProbeSideOptimizer`): hash the smaller side.
fn assign_build_sides<S: Stats + ?Sized>(model: &CostModel<'_, S>, spec: &mut JoinSpec) {
    if let JoinSpec::HashJoin { build, probe } = spec {
        assign_build_sides(model, build);
        assign_build_sides(model, probe);
        let mask_of = |s: &JoinSpec| s.patterns().iter().fold(0 as Mask, |m, &i| m | (1 << i));
        if model.card(mask_of(build)) > model.card(mask_of(probe)) {
            std::mem::swap(build, probe);
        }
    }
}
