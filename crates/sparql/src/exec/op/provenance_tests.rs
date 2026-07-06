//! Unit tests for `Op::may_emit_term` — the static per-column provenance
//! claim the streaming joins use to pick forced-decode columns (#128).

use crate::algebra::{Expr, Term, TriplePattern, Var};
use crate::exec::horn::HornBackend;
use crate::exec::runtime::Runtime;
use crate::exec::Store;
use crate::plan::PhysicalPlan;

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}

fn cell(s: &str) -> Option<Term> {
    Some(iri(s))
}

fn scan_plan() -> PhysicalPlan {
    PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("s")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    }
}

fn one_triple_store() -> HornBackend {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("s"), iri("p"), iri("o"));
    horn
}

/// BGP scan columns come straight from the dictionary: never Term.
#[test]
fn scan_columns_never_emit_term() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let op = rt.build(&scan_plan()).unwrap();
    assert_eq!(op.schema().len(), 2);
    assert_eq!(op.may_emit_term(), vec![false; 2]);
}

/// VALUES cells are Slot::Term (or Unbound): every column may emit Term.
#[test]
fn values_columns_may_emit_term() {
    let horn = HornBackend::new();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x"), Var::new("y")],
        rows: vec![vec![cell("a"), None]],
    };
    let op = rt.build(&plan).unwrap();
    assert_eq!(op.may_emit_term(), vec![true, true]);
}

/// BIND marks only its output column; inherited scan columns stay Id-only.
#[test]
fn extend_marks_only_the_bind_column() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Extend {
        inner: Box::new(scan_plan()),
        var: Var::new("x"),
        expr: Expr::Term(iri("c")),
    };
    let op = rt.build(&plan).unwrap();
    let terms = op.may_emit_term();
    for (i, v) in op.schema().iter().enumerate() {
        assert_eq!(terms[i], v.name() == "x", "column ?{}", v.name());
    }
}

/// A join ORs its children per column: scan-only columns stay false,
/// Values-fed columns (including a shared one) become true.
#[test]
fn join_ors_children_per_column() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let values = PhysicalPlan::Values {
        vars: vec![Var::new("o"), Var::new("z")],
        rows: vec![vec![cell("o"), cell("z1")]],
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(scan_plan()),
        right: Box::new(values),
    };
    let op = rt.build(&plan).unwrap();
    let terms = op.may_emit_term();
    for (i, v) in op.schema().iter().enumerate() {
        let want = matches!(v.name(), "o" | "z"); // the Values side feeds ?o and ?z
        assert_eq!(terms[i], want, "column ?{}", v.name());
    }
}

/// GROUP BY: key columns inherit child provenance, aggregate outputs are
/// computed terms.
#[test]
fn group_keys_inherit_aggregates_are_term() {
    use crate::algebra::{AggFunc, Aggregate};
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Group {
        inner: Box::new(scan_plan()),
        keys: vec![Var::new("s")],
        aggregates: vec![Aggregate {
            out: Var::new("cnt"),
            func: AggFunc::CountStar,
            distinct: false,
        }],
    };
    let op = rt.build(&plan).unwrap();
    // Schema is [?s, ?cnt] (group_output_schema: keys ++ aggregate outs).
    assert_eq!(op.may_emit_term(), vec![false, true]);
}
