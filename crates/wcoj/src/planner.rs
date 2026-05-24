//! Cost-based plan choice between WCOJ and binary-hash.
//!
//! Stage-1 heuristic (SPEC-03 F2): default cutover is 4 patterns. For ≤3
//! patterns, binary-hash. For ≥4, WCOJ. The cardinality estimator is
//! retained as the seam where Stage-2 cost-based logic (estimator-driven
//! join-order selection and per-pattern ordering choice) will land.

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
