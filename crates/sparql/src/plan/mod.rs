//! Physical plan tree. Each node is one work unit the runtime
//! executes against an [`crate::exec::Executor`].

pub mod planner;

use crate::algebra::{Expr, OrderDir, Term, TriplePattern, Var};

#[derive(Debug, Clone, PartialEq)]
pub enum PhysicalPlan {
    /// Leaf: scan a BGP via the executor.
    BgpScan {
        patterns: Vec<TriplePattern>,
    },
    /// Cartesian/equi-join of two child plans on shared variables.
    Join {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
    },
    /// Left-outer-join, optional ON expression.
    LeftJoin {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        expr: Option<Expr>,
    },
    /// Filter rows by a boolean expression.
    Filter {
        expr: Expr,
        inner: Box<PhysicalPlan>,
    },
    /// UNION of two compatible plans.
    Union {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
    },
    /// Restrict output columns.
    Project {
        vars: Vec<Var>,
        inner: Box<PhysicalPlan>,
    },
    /// Deduplicate rows.
    Distinct {
        inner: Box<PhysicalPlan>,
    },
    /// OFFSET/LIMIT.
    Slice {
        inner: Box<PhysicalPlan>,
        start: usize,
        length: Option<usize>,
    },
    /// ORDER BY.
    OrderBy {
        inner: Box<PhysicalPlan>,
        keys: Vec<(Expr, OrderDir)>,
    },
    /// BIND.
    Extend {
        inner: Box<PhysicalPlan>,
        var: Var,
        expr: Expr,
    },
    /// VALUES row source.
    Values {
        vars: Vec<Var>,
        rows: Vec<Vec<Option<Term>>>,
    },
}
