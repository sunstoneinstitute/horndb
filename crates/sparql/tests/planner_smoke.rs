use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};
use horndb_sparql::plan::{planner, PhysicalPlan};

fn bgp(pat: TriplePattern) -> Algebra {
    Algebra::Bgp {
        patterns: vec![pat],
    }
}

#[test]
fn plans_bgp_as_scan() {
    let alg = bgp(TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    });
    let plan = planner::plan(&alg).expect("plan");
    assert!(matches!(plan, PhysicalPlan::BgpScan { .. }));
}

#[test]
fn plans_project_over_bgp() {
    let inner = bgp(TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    });
    let alg = Algebra::Project {
        vars: vec![Var::new("s")],
        inner: Box::new(inner),
    };
    let plan = planner::plan(&alg).expect("plan");
    // The root Project carries the query's projection; ProjectionPushdown
    // may nest further restricting Projects below it, so walk through them
    // to the scan.
    let PhysicalPlan::Project { vars, inner } = plan else {
        panic!("expected Project root, got a non-Project plan")
    };
    assert_eq!(vars.len(), 1);
    let mut node = &*inner;
    while let PhysicalPlan::Project { inner, .. } = node {
        node = inner;
    }
    assert!(
        matches!(node, PhysicalPlan::BgpScan { .. }),
        "Project chain must bottom out at a BgpScan, got {node:?}"
    );
}
