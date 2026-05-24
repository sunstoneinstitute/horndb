use reasoner_wcoj::ids::{Ordering, Triple};
use reasoner_wcoj::pattern::{Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;
use reasoner_wcoj::trie::leapfrog::LeapfrogJoin;
use reasoner_wcoj::trie::source_iter::PatternTrieIter;
use reasoner_wcoj::trie::TrieIterator;

#[test]
fn leapfrog_intersection_returns_only_common_values() {
    // Three patterns sharing variable ?x at level 0:
    //   (?x, p, 1)  → ?x ∈ {a where (a, p, 1)}
    //   (?x, q, 2)  → ?x ∈ {a where (a, q, 2)}
    //   (?x, r, 3)  → ?x ∈ {a where (a, r, 3)}
    // Subjects matching all three should be exactly {7}.
    let triples = vec![
        // x=5 matches p,q but not r
        Triple::new(5, 10, 1),
        Triple::new(5, 20, 2),
        // x=7 matches all three
        Triple::new(7, 10, 1),
        Triple::new(7, 20, 2),
        Triple::new(7, 30, 3),
        // x=9 matches r,q but not p
        Triple::new(9, 20, 2),
        Triple::new(9, 30, 3),
    ];
    let src = VecTripleSource::from_triples(triples);
    let var_order = vec![Var(0)];

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1));
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Bound(2));
    let p3 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(30), Term::Bound(3));

    let it1 = PatternTrieIter::new(&src, &p1, &var_order, Ordering::Pos).unwrap();
    let it2 = PatternTrieIter::new(&src, &p2, &var_order, Ordering::Pos).unwrap();
    let it3 = PatternTrieIter::new(&src, &p3, &var_order, Ordering::Pos).unwrap();

    let iters: Vec<Box<dyn TrieIterator>> =
        vec![Box::new(it1), Box::new(it2), Box::new(it3)];

    let mut join = LeapfrogJoin::new(iters, 0);
    let mut out = Vec::new();
    while let Some(v) = join.next() {
        out.push(v);
    }
    assert_eq!(out, vec![7]);
}

#[test]
fn leapfrog_empty_when_one_iterator_is_empty() {
    let triples = vec![Triple::new(5, 10, 1), Triple::new(5, 20, 2)];
    let src = VecTripleSource::from_triples(triples);
    let var_order = vec![Var(0)];

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1));
    // No triple has p=99
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Bound(2));
    let it1 = PatternTrieIter::new(&src, &p1, &var_order, Ordering::Pos).unwrap();
    let it2 = PatternTrieIter::new(&src, &p2, &var_order, Ordering::Pos).unwrap();
    let iters: Vec<Box<dyn TrieIterator>> = vec![Box::new(it1), Box::new(it2)];
    let mut join = LeapfrogJoin::new(iters, 0);
    assert_eq!(join.next(), None);
}
