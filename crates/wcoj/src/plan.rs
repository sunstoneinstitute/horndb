//! Plan IR for one BGP.
//!
//! Two layers:
//!
//! - [`JoinSpec`] — the per-BGP join plan the planner emits (SPEC-23 §5.5):
//!   a tree of binary hash joins whose leaves are single-pattern scans or
//!   multi-way leapfrog (WCOJ) nodes. A whole-BGP plan is one `Wcoj` node;
//!   a pure binary plan is a tree of `Scan`s; anything in between is the
//!   Freitag structural hybrid (WCOJ only where a cyclic core needs it).
//! - [`ExecutionPlan`] — what one WCOJ node hands the leapfrog executor:
//!   its variable elimination order. `PlanKind::BinaryHash` survives only as
//!   the legacy fixed-cutover route (`HORNDB_WCOJ_CUTOVER`, for bisection).

use crate::pattern::{Bgp, Var};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    /// Leapfrog Triejoin over the whole BGP.
    Wcoj,
    /// Left-deep binary hash join in pattern order (legacy cutover route and
    /// the fully-ground degenerate case).
    BinaryHash,
}

/// Input to the leapfrog executor: the variable elimination order of one
/// WCOJ node (depth 0 = outermost).
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub kind: PlanKind,
    pub var_order: Vec<Var>,
}

/// Per-BGP join plan. Pattern indices refer to `Bgp::patterns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinSpec {
    /// One multi-way leapfrog over `patterns`, eliminating variables in
    /// `var_order` (every variable those patterns mention, each once).
    Wcoj {
        patterns: Vec<usize>,
        var_order: Vec<Var>,
    },
    /// Materialise the matches of one pattern.
    Scan { pattern: usize },
    /// Hash join: build a table on `build`, probe it with `probe`. Join keys
    /// are the variables both sides bind; none means a cross product.
    HashJoin {
        build: Box<JoinSpec>,
        probe: Box<JoinSpec>,
    },
}

impl JoinSpec {
    /// Pattern indices this subtree covers, ascending.
    pub fn patterns(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_patterns(&mut out);
        out.sort_unstable();
        out
    }

    fn collect_patterns(&self, out: &mut Vec<usize>) {
        match self {
            JoinSpec::Wcoj { patterns, .. } => out.extend_from_slice(patterns),
            JoinSpec::Scan { pattern } => out.push(*pattern),
            JoinSpec::HashJoin { build, probe } => {
                build.collect_patterns(out);
                probe.collect_patterns(out);
            }
        }
    }

    /// Variables this subtree binds: a `Wcoj` node's elimination order, a
    /// `Scan`'s pattern variables in S/P/O order, a join's build-side
    /// variables followed by the probe side's new ones.
    pub fn vars(&self, bgp: &Bgp) -> Vec<Var> {
        match self {
            JoinSpec::Wcoj { var_order, .. } => var_order.clone(),
            JoinSpec::Scan { pattern } => {
                let p = &bgp.patterns[*pattern];
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
            JoinSpec::HashJoin { build, probe } => {
                let mut out = build.vars(bgp);
                for v in probe.vars(bgp) {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
                out
            }
        }
    }

    /// The elimination order if this plan is one WCOJ node over the whole
    /// BGP — the case the streaming leapfrog executor runs directly.
    pub fn as_whole_wcoj(&self, bgp: &Bgp) -> Option<&[Var]> {
        match self {
            JoinSpec::Wcoj {
                patterns,
                var_order,
            } if patterns.len() == bgp.patterns.len() => Some(var_order),
            _ => None,
        }
    }

    /// Left-deep hash-join chain over `patterns` in the given order — the
    /// shape the binary-hash oracle and the legacy cutover route use.
    pub fn left_deep(patterns: impl IntoIterator<Item = usize>) -> Option<JoinSpec> {
        let mut it = patterns.into_iter();
        let mut acc = JoinSpec::Scan {
            pattern: it.next()?,
        };
        for pattern in it {
            acc = JoinSpec::HashJoin {
                build: Box::new(JoinSpec::Scan { pattern }),
                probe: Box::new(acc),
            };
        }
        Some(acc)
    }

    /// Lift a legacy whole-BGP [`ExecutionPlan`] into a `JoinSpec`.
    pub fn from_execution_plan(plan: &ExecutionPlan, bgp: &Bgp) -> JoinSpec {
        let all = 0..bgp.patterns.len();
        match plan.kind {
            PlanKind::Wcoj => JoinSpec::Wcoj {
                patterns: all.collect(),
                var_order: plan.var_order.clone(),
            },
            PlanKind::BinaryHash => JoinSpec::left_deep(all).unwrap_or(JoinSpec::Wcoj {
                patterns: Vec::new(),
                var_order: Vec::new(),
            }),
        }
    }
}

impl ExecutionPlan {
    /// The retired Stage-1 heuristic: `>= wcoj_cutover` patterns go WCOJ,
    /// fewer go binary-hash, and variables are eliminated by descending
    /// pattern degree (how many patterns mention them), ties in
    /// first-appearance order. Kept reachable through
    /// `HORNDB_WCOJ_CUTOVER` so a planner regression can be bisected against
    /// it; the cost-based planner lives in [`crate::planner`].
    pub fn for_bgp(bgp: &Bgp, wcoj_cutover: usize) -> Self {
        // Ground BGPs are degenerate — pick BinaryHash; the executor will
        // short-circuit them.
        let all_ground = bgp.patterns.iter().all(|p| p.is_ground());
        if all_ground {
            return Self {
                kind: PlanKind::BinaryHash,
                var_order: Vec::new(),
            };
        }

        let kind = if bgp.patterns.len() >= wcoj_cutover {
            PlanKind::Wcoj
        } else {
            PlanKind::BinaryHash
        };

        Self {
            kind,
            var_order: degree_order(bgp),
        }
    }
}

/// Variables by descending pattern degree, ties by first appearance — the
/// structural order used when statistics carry no signal.
pub fn degree_order(bgp: &Bgp) -> Vec<Var> {
    let mut degrees: Vec<(Var, usize)> = bgp
        .variables()
        .into_iter()
        .map(|v| {
            let d = bgp
                .patterns
                .iter()
                .filter(|p| p.position_of(v).is_some())
                .count();
            (v, d)
        })
        .collect();
    // Stable sort: first-appearance order survives ties.
    degrees.sort_by(|a, b| b.1.cmp(&a.1));
    degrees.into_iter().map(|(v, _)| v).collect()
}
