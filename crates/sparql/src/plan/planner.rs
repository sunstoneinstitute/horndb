//! Algebra → PhysicalPlan.
//!
//! Stage 1 is a thin 1:1 lowering. There is **no** cost model: BGP
//! patterns are sent to the executor in the textual order they appear,
//! and the executor (or, later, SPEC-03) is responsible for join
//! ordering. This avoids us building a planner we'd just throw away.

use crate::algebra::Algebra;
use crate::error::Result;
use crate::plan::PhysicalPlan;

pub fn plan(alg: &Algebra) -> Result<PhysicalPlan> {
    Ok(match alg {
        Algebra::Bgp { patterns } => PhysicalPlan::BgpScan {
            patterns: patterns.clone(),
        },
        Algebra::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
        },
        Algebra::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
            expr: expr.clone(),
        },
        Algebra::Filter { expr, inner } => PhysicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(plan(inner)?),
        },
        Algebra::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
        },
        Algebra::Project { vars, inner } => PhysicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(plan(inner)?),
        },
        Algebra::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(plan(inner)?),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => PhysicalPlan::Slice {
            inner: Box::new(plan(inner)?),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(plan(inner)?),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(plan(inner)?),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => PhysicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => PhysicalPlan::Group {
            inner: Box::new(plan(inner)?),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PhysicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(plan(edge)?),
            reflexive: *reflexive,
        },
    })
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
            PhysicalPlan::Join { .. } => {}
            other => panic!("expected Join, got {other:?}"),
        }
    }

    // Compile-time witness that SparqlError is the error path so we
    // don't accidentally start panicking on lower failures.
    #[allow(dead_code)]
    fn err_path() -> Result<PhysicalPlan> {
        Err(SparqlError::Planner("never".into()))
    }
}
