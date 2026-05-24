use reasoner_wcoj::cardinality::UniformEstimator;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::PlanKind;
use reasoner_wcoj::planner::Planner;
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn four_pattern_cycle_picks_wcoj() {
    let p = 10;
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let src = VecTripleSource::from_triples(vec![Triple::new(1, p, 2)]);
    let est = UniformEstimator::from_source(&src);
    let planner = Planner::default();
    let plan = planner.choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::Wcoj);
}

#[test]
fn two_pattern_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(20), Term::Var(Var(2))),
    ]);
    let src = VecTripleSource::from_triples(vec![]);
    let est = UniformEstimator::from_source(&src);
    let plan = Planner::default().choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}

#[test]
fn three_patterns_with_low_cardinality_picks_binary_hash() {
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Bound(1), Term::Bound(10), Term::Var(Var(0))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(30), Term::Bound(99)),
    ]);
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let est = UniformEstimator::from_source(&src);
    let plan = Planner::default().choose(&bgp, &est);
    assert_eq!(plan.kind, PlanKind::BinaryHash);
}
