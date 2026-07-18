//! Physical plan tree. Each node is one work unit the runtime
//! executes against an [`crate::exec::Executor`].

pub mod explain;
pub mod logical;
pub mod lower;
pub mod planner;
pub mod pushdown;
pub mod types;

use crate::algebra::{Aggregate, Expr, OrderDir, Term, TriplePattern, Var};

#[derive(Debug, Clone, PartialEq)]
pub enum PhysicalPlan {
    /// Leaf: scan a BGP via the executor.
    BgpScan { patterns: Vec<TriplePattern> },
    /// Pushed-down COUNT over a BGP (#144): yields one row binding `out_var` to
    /// the solution count, without materializing rows. Falls back to scan+count
    /// when the backend has no fast `count_bgp`.
    CountScan {
        patterns: Vec<TriplePattern>,
        out_var: Var,
    },
    /// Pushed-down grouped / multi-output COUNT over a BGP (#128): one row
    /// per group — the key slots followed by one `xsd:integer` count per
    /// `out_vars` entry. Every aggregate this node replaces is a plain
    /// (non-DISTINCT) count of the group size, so all outputs carry the same
    /// number. `keys` may be empty (implicit grouping with ≥2 counts). Falls
    /// back to scan + hash-count when the backend has no fast
    /// `count_bgp_grouped`. Output rows are sorted by the decoded lexical
    /// form of the key slots — the same order the streaming `Group` emits.
    /// RDF 1.2 triple-term keys all lex-serialize to "" (Stage-1 limitation),
    /// so their relative group order is unspecified — on both this leaf and
    /// the streaming path.
    GroupCountScan {
        patterns: Vec<TriplePattern>,
        keys: Vec<Var>,
        out_vars: Vec<Var>,
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
    Distinct { inner: Box<PhysicalPlan> },
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
    /// GROUP BY + aggregates.
    Group {
        inner: Box<PhysicalPlan>,
        keys: Vec<Var>,
        aggregates: Vec<Aggregate>,
    },
    /// Recursive Kleene property path `p+`/`p*`. `edge` produces the
    /// one-step relation over the hidden endpoint variables
    /// (`?pp_src`, `?pp_dst`); the runtime takes its transitive (and,
    /// when `reflexive`, reflexive) closure and binds the result to
    /// `subject`/`object`. See [`crate::algebra::Algebra::PathClosure`].
    PathClosure {
        subject: Term,
        object: Term,
        edge: Box<PhysicalPlan>,
        reflexive: bool,
    },
}
