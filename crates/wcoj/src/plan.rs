//! Execution plan: chooses between WCOJ and binary-hash, and picks the
//! variable ordering used by Leapfrog Triejoin.

use crate::pattern::{Bgp, Var};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    /// Leapfrog Triejoin — for ≥`wcoj_cutover` patterns.
    Wcoj,
    /// Left-deep binary hash join — for ≤`wcoj_cutover - 1` patterns
    /// and for fully-ground BGPs.
    BinaryHash,
}

#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub kind: PlanKind,
    /// Variable elimination order for WCOJ (depth 0 = outermost).
    pub var_order: Vec<Var>,
}

impl ExecutionPlan {
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

        // Order variables by descending degree (how many patterns mention
        // them). High-degree first cuts the search space fastest. Ties
        // broken by first-appearance order for determinism.
        let vars = bgp.variables();
        let mut degrees: Vec<(Var, usize)> = vars
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
        // Stable sort by descending degree; first-appearance order survives ties.
        degrees.sort_by(|a, b| b.1.cmp(&a.1));
        let var_order = degrees.into_iter().map(|(v, _)| v).collect();

        Self { kind, var_order }
    }
}
