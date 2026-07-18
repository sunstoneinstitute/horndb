//! Algebra → PhysicalPlan, via the logical IR + pass pipeline (SPEC-23 §5).
//!
//! Phase 1 wires `Algebra → LogicalPlan → run_passes → PhysicalPlan`. The
//! only registered pass is `CoalesceBgp` (SPEC-23 §5.1): it folds
//! `Join(Bgp, Bgp)` into one flat BGP. spargebra already merges adjacent
//! triple patterns, so on most queries the pass is a no-op and the emitted
//! plan is structurally identical to the pre-refactor 1:1 lowering (the
//! golden-plan gate). The exception is the Stage-1 `GRAPH` lowering
//! (merged-graph semantics): a query mixing top-level triples with a
//! `GRAPH` block produces `Join(Bgp, Bgp)`, which now coalesces into one
//! flat `BgpScan` — result-invariant, and disable-able via
//! `PRAGMA disable-pass=coalesce-bgp`. Cost-based ordering and the
//! heuristic rewrite passes land in later phases behind the same registry.

use crate::algebra::Algebra;
use crate::error::Result;
use crate::plan::lower::{lower_algebra, lower_physical};
use crate::plan::pass::{run_passes, standard_passes, PlanCtx};
use crate::plan::PhysicalPlan;

/// Plan `alg` with the default context (no passes disabled).
pub fn plan(alg: &Algebra) -> Result<PhysicalPlan> {
    plan_with_ctx(alg, &PlanCtx::default())
}

/// Plan `alg` under an explicit [`PlanCtx`] (e.g. with passes disabled by a
/// query pragma). Lowers to the logical IR, runs the pass pipeline, then
/// lowers to the physical plan.
pub fn plan_with_ctx(alg: &Algebra, ctx: &PlanCtx) -> Result<PhysicalPlan> {
    let logical = lower_algebra(alg);
    let optimized = run_passes(logical, &standard_passes(), ctx);
    Ok(lower_physical(optimized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::error::SparqlError;

    #[test]
    fn empty_bgp_plans_to_empty_scan() {
        let plan = plan(&Algebra::Bgp { patterns: vec![] }).unwrap();
        assert_eq!(plan, PhysicalPlan::BgpScan { patterns: vec![] });
    }

    #[test]
    fn join_lowers_both_sides() {
        // Non-coalescible children (right side is not a bare BGP), so the
        // Join survives the pipeline and lowers both sides.
        let bgp = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: Term::Iri("p".into()),
                object: Term::Var(Var::new("o")),
            }],
        };
        let alg = Algebra::Join {
            left: Box::new(bgp.clone()),
            right: Box::new(Algebra::Distinct {
                inner: Box::new(bgp),
            }),
        };
        match plan(&alg).unwrap() {
            PhysicalPlan::Join { .. } => {}
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn join_of_bare_bgps_coalesces_to_one_flat_scan() {
        // Hand-built Join(Bgp, Bgp) — spargebra never emits this shape — is
        // folded by the CoalesceBgp pass into a single flat BgpScan.
        let bgp = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: Term::Iri("p".into()),
                object: Term::Var(Var::new("o")),
            }],
        };
        let alg = Algebra::Join {
            left: Box::new(bgp.clone()),
            right: Box::new(bgp),
        };
        match plan(&alg).unwrap() {
            PhysicalPlan::BgpScan { patterns } => assert_eq!(patterns.len(), 2),
            other => panic!("expected coalesced BgpScan, got {other:?}"),
        }
    }

    // Compile-time witness that SparqlError is the error path so we
    // don't accidentally start panicking on lower failures.
    #[allow(dead_code)]
    fn err_path() -> Result<PhysicalPlan> {
        Err(SparqlError::Planner("never".into()))
    }
}
