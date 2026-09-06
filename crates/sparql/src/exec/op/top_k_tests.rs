//! Which plan shapes `Runtime::build_top_k` fuses (HDB-101).
//!
//! The row-order parity tests live with the sort itself
//! (`exec::runtime`'s `top_k_*` tests); these pin the *shape predicate*, so a
//! parity test cannot quietly pass by never taking the fused path at all.

use crate::algebra::{Expr, OrderDir, Term, Var};
use crate::exec::horn::HornBackend;
use crate::exec::runtime::Runtime;
use crate::plan::PhysicalPlan;

fn values() -> PhysicalPlan {
    PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: vec![vec![Some(Term::Iri("http://ex/a".into()))]],
    }
}

fn order_by(inner: PhysicalPlan) -> PhysicalPlan {
    PhysicalPlan::OrderBy {
        inner: Box::new(inner),
        keys: vec![(Expr::Term(Term::Var(Var::new("x"))), OrderDir::Asc)],
    }
}

/// Whether `build_top_k` accepts this shape.
fn fuses(plan: &PhysicalPlan) -> bool {
    let horn = HornBackend::new();
    let rt = Runtime::new(&horn);
    let fused = rt.build_top_k(plan, 10, &[]).unwrap().is_some();
    fused
}

#[test]
fn fuses_order_by_directly_under_the_slice() {
    assert!(fuses(&order_by(values())));
}

/// SPARQL algebra puts the sort under the projection, so this — not the bare
/// `OrderBy` — is the shape a plain `ORDER BY .. LIMIT ..` actually produces.
#[test]
fn fuses_through_a_projection() {
    let plan = PhysicalPlan::Project {
        vars: vec![Var::new("x")],
        inner: Box::new(order_by(values())),
    };
    assert!(fuses(&plan));
}

/// Every shape that drops rows after the sort must block the fusion: `n`
/// sorted rows are no longer enough to answer the limit.
#[test]
fn refuses_shapes_that_drop_rows_after_the_sort() {
    let distinct = PhysicalPlan::Distinct {
        inner: Box::new(order_by(values())),
    };
    assert!(!fuses(&distinct), "Distinct above OrderBy must not fuse");

    let filter = PhysicalPlan::Filter {
        expr: Expr::Bound(Var::new("x")),
        inner: Box::new(order_by(values())),
    };
    assert!(!fuses(&filter), "Filter above OrderBy must not fuse");

    assert!(!fuses(&values()), "no OrderBy at all must not fuse");
}

/// The end-to-end check that the query shape this task targets really reaches
/// the fused path — planner output included, not a hand-built plan.
#[test]
fn planned_order_by_limit_query_fuses() {
    use crate::algebra::translate::translate_query_with;
    use crate::parser::{parse_query, ParsedQuery};
    use crate::plan::planner;
    use crate::SparqlConfig;

    let q = "SELECT ?s ?a WHERE { ?s <http://ex/amount> ?a } ORDER BY DESC(?a) LIMIT 50";
    let ParsedQuery::Select { inner } = parse_query(q).unwrap() else {
        panic!("expected SELECT");
    };
    let translated = translate_query_with(&inner, &SparqlConfig::default()).unwrap();
    let plan = planner::plan(&translated.algebra).unwrap();

    // The planner wraps the whole thing in an outer Project; the fusable
    // shape is `Slice(Project(OrderBy(..)))` one level down.
    let PhysicalPlan::Project { inner, .. } = &plan else {
        panic!("expected a Project at the root, got {plan:?}");
    };
    let PhysicalPlan::Slice { inner, .. } = &**inner else {
        panic!("expected a Slice under the root Project, got {plan:?}");
    };
    assert!(
        fuses(inner),
        "ORDER BY .. LIMIT .. did not reach the top-k fusion; plan was {plan:?}"
    );
}
