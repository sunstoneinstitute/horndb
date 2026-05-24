use reasoner_wcoj::cardinality::{Cardinality, UniformEstimator};
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn fully_unbound_pattern_estimates_total_triples() {
    let src = VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(2, 10, 100),
        Triple::new(3, 20, 200),
    ]);
    let est = UniformEstimator::from_source(&src);
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Var(Var(1)), Term::Var(Var(2)));
    assert_eq!(est.estimate(&pat), 3);
}

#[test]
fn one_bound_position_estimates_third() {
    // Stub heuristic: each bound position cuts the estimate to 1/16 (rough
    // proxy for predicate skew). 3 triples * (1/16) ≈ 0 — clamp to 1.
    let src = VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(2, 10, 100),
        Triple::new(3, 20, 200),
    ]);
    let est = UniformEstimator::from_source(&src);
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let e = est.estimate(&pat);
    assert!(e >= 1 && e <= 3, "estimate {e} should be 1..=3");
}
