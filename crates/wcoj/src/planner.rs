//! Cost-based plan choice between WCOJ and binary-hash.
//!
//! Stage-1 heuristic: default cutover is 4 patterns (SPEC-03 F2). For ≤3
//! patterns, binary-hash. For ≥4, WCOJ. The cardinality estimator is
//! retained as the seam where Stage-2 cost-based logic (estimator-driven
//! join-order selection and per-pattern ordering choice) will land — that
//! is **SPEC-03 F6**, not F2, and it is unmet: `choose` drops `_est`, and
//! `ExecutionPlan::for_bgp` orders variables by descending pattern degree
//! with no cardinality input. HDB-108 measured what that costs on
//! trainmarks q3 (`docs/benchmarks.md`); the fix is HDB-46 /
//! `docs/plans/PLAN-23-04-cost-based-join-planning.md` Task 5.

use crate::cardinality::Cardinality;
use crate::pattern::Bgp;
use crate::plan::ExecutionPlan;

pub struct Planner {
    pub wcoj_cutover: usize,
}

impl Default for Planner {
    fn default() -> Self {
        Self { wcoj_cutover: 4 }
    }
}

impl Planner {
    pub fn choose<C: Cardinality>(&self, bgp: &Bgp, _est: &C) -> ExecutionPlan {
        ExecutionPlan::for_bgp(bgp, self.wcoj_cutover)
    }
}
