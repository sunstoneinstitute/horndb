use reasoner_wcoj::ids::Ordering;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};

#[test]
fn variables_collects_unique_vars_in_first_appearance_order() {
    // ?a p ?b . ?b q ?c . ?a r ?c
    let p = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let q = TriplePattern::new(Term::Var(Var(1)), Term::Bound(11), Term::Var(Var(2)));
    let r = TriplePattern::new(Term::Var(Var(0)), Term::Bound(12), Term::Var(Var(2)));
    let bgp = Bgp::new(vec![p, q, r]);

    let vars = bgp.variables();
    assert_eq!(vars, vec![Var(0), Var(1), Var(2)]);
}

#[test]
fn pattern_with_all_three_bound_is_ground() {
    let g = TriplePattern::new(Term::Bound(1), Term::Bound(2), Term::Bound(3));
    assert!(g.is_ground());
    let v = TriplePattern::new(Term::Var(Var(0)), Term::Bound(2), Term::Bound(3));
    assert!(!v.is_ground());
}

#[test]
fn preferred_ordering_puts_bound_positions_first() {
    // Pattern (?s, p_bound, ?o) — we want predicate at level 0 so the
    // executor can seek to it immediately. Result: PSO or POS.
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(42), Term::Var(Var(1)));
    let ord = pat.preferred_ordering();
    assert!(matches!(ord, Ordering::Pso | Ordering::Pos));
}
