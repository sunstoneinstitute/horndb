use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};

#[test]
fn plan_for_4_cycle_uses_wcoj_and_orders_vars_by_degree() {
    // 4-cycle: (?a, p, ?b)(?b, p, ?c)(?c, p, ?d)(?d, p, ?a)
    let p = 10;
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan::for_bgp(&bgp, 4);
    assert_eq!(plan.kind, PlanKind::Wcoj);
    // All 4 variables present.
    assert_eq!(plan.var_order.len(), 4);
    let mut sorted = plan.var_order.clone();
    sorted.sort();
    assert_eq!(sorted, vec![Var(0), Var(1), Var(2), Var(3)]);
}

#[test]
fn plan_for_two_pattern_bgp_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
    ]);
    let plan = ExecutionPlan::for_bgp(&bgp, 4);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}
