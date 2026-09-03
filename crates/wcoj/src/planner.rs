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
//!    subsets (DPccp restricted to core-respecting subsets). DuckDB's dual
//!    guard — a pattern-count threshold and a work budget — falls back to a
//!    greedy build-up. Hash build sides are assigned in a late pass so the
//!    DP state stays symmetric.
//!
//! Statistics with no per-predicate signal (`Stats::is_informed() == false`)
//! skip the cost search: the whole BGP goes to one WCOJ node in structural
//! degree order, the production path. `HORNDB_WCOJ_CUTOVER=<n>` restores
//! the retired Stage-1 pattern-count rule for bisection.

use std::sync::OnceLock;

use crate::cost::{CostModel, Mask};
use crate::pattern::Bgp;
use crate::plan::{ExecutionPlan, JoinSpec};
use crate::stats::Stats;

/// Past this many non-ground patterns the DP is skipped for the greedy.
pub const MAX_DP_PATTERNS: usize = 10;
/// Subset-pair visits the DP may spend before falling back to the greedy.
pub const DP_WORK_BUDGET: usize = 100_000;

#[derive(Debug, Clone)]
pub struct Planner {
    pub max_dp_patterns: usize,
    pub dp_work_budget: usize,
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
            dp_work_budget: DP_WORK_BUDGET,
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
            // Fully ground: membership tests only, the binary path's job.
            return JoinSpec::from_execution_plan(&ExecutionPlan::for_bgp(bgp, usize::MAX), bgp);
        }
        let model = CostModel::new(bgp, stats);
        if !model.informed() || n > Mask::BITS as usize {
            return whole_wcoj(&model, &live, &ground);
        }
        let core: Mask = model
            .cyclic_core(&live)
            .iter()
            .fold(0, |m, &i| m | (1 << i));
        let search = Search {
            model: &model,
            live: &live,
            core,
            adj: adjacency(bgp, &live),
        };
        let mut spec = if live.len() <= self.max_dp_patterns {
            search.dp(self.dp_work_budget)
        } else {
            None
        }
        .unwrap_or_else(|| search.greedy());
        assign_build_sides(&model, &mut spec);
        attach_ground(&model, spec, &live, &ground)
    }
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
    core: Mask,
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

    /// Freitag's rule: a subset either holds the whole cyclic core or none
    /// of it, so no hash join ever splits the core.
    fn respects_core(&self, mask: Mask) -> bool {
        mask & self.core == 0 || mask & self.core == self.core
    }

    fn wcoj_node(&self, mask: Mask) -> (f64, JoinSpec) {
        let patterns = self.members(mask);
        let c = self.model.wcoj(&patterns);
        (
            c.cost,
            JoinSpec::Wcoj {
                patterns,
                var_order: c.var_order,
            },
        )
    }

    /// DP over connected, core-respecting subsets. `None` when the work
    /// budget runs out or the BGP is disconnected (a cross product the
    /// leapfrog handles natively).
    fn dp(&self, budget: usize) -> Option<JoinSpec> {
        let full: Mask = self.live.iter().fold(0, |m, &i| m | (1 << i));
        let mut best: std::collections::HashMap<Mask, (f64, JoinSpec)> =
            std::collections::HashMap::new();
        let mut work = 0usize;
        // Enumerate submasks of `full` in increasing popcount so every
        // split's halves are already solved.
        let mut masks: Vec<Mask> = submasks(full).collect();
        masks.sort_by_key(|m| m.count_ones());
        for mask in masks {
            if mask == 0 || !self.connected(mask) || !self.respects_core(mask) {
                continue;
            }
            let (mut cost, mut spec) = if mask.count_ones() == 1 {
                let p = mask.trailing_zeros() as usize;
                let wcoj = self.wcoj_node(mask);
                let scan = (self.model.scan_cost(p), JoinSpec::Scan { pattern: p });
                if wcoj.0 <= scan.0 {
                    wcoj
                } else {
                    scan
                }
            } else {
                self.wcoj_node(mask)
            };
            // Every split into two solved halves that share a variable.
            let rest = mask & (mask - 1); // drop the lowest bit
            for a in submasks(rest) {
                work += 1;
                if work > budget {
                    return None;
                }
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

    /// Greedy build-up: seed with the cyclic core as one WCOJ node (or the
    /// cheapest scan), then repeatedly hash-join the connected pattern whose
    /// scan costs least; compare against one WCOJ node over everything.
    fn greedy(&self) -> JoinSpec {
        let full: Mask = self.live.iter().fold(0, |m, &i| m | (1 << i));
        let (whole_cost, whole) = self.wcoj_node(full);
        let (mut cost, mut spec, mut mask) = if self.core != 0 {
            let (c, s) = self.wcoj_node(self.core);
            (c, s, self.core)
        } else {
            let p = *self
                .live
                .iter()
                .min_by(|&&a, &&b| {
                    self.model
                        .scan_cost(a)
                        .partial_cmp(&self.model.scan_cost(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("live patterns");
            (
                self.model.scan_cost(p),
                JoinSpec::Scan { pattern: p },
                1 << p,
            )
        };
        while mask != full {
            let mut candidates = self.neighbours(mask) & full;
            if candidates == 0 {
                candidates = full & !mask; // disconnected: cross product
            }
            let p = self
                .members(candidates)
                .into_iter()
                .min_by(|&a, &b| {
                    self.model
                        .join_cost(mask, 1 << a)
                        .partial_cmp(&self.model.join_cost(mask, 1 << b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("candidate pattern");
            cost += self.model.scan_cost(p) + self.model.join_cost(mask, 1 << p);
            spec = JoinSpec::HashJoin {
                build: Box::new(JoinSpec::Scan { pattern: p }),
                probe: Box::new(spec),
            };
            mask |= 1 << p;
        }
        if whole_cost <= cost {
            whole
        } else {
            spec
        }
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
