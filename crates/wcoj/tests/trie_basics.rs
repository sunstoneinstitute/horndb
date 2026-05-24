use horndb_wcoj::ids::{Ordering, Triple};
use horndb_wcoj::pattern::{Term, TriplePattern, Var};
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::trie::source_iter::PatternTrieIter;
use horndb_wcoj::trie::TrieIterator;

fn source() -> VecTripleSource {
    VecTripleSource::from_triples(vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 300),
        Triple::new(2, 10, 100),
        Triple::new(2, 10, 400),
    ])
}

#[test]
fn pattern_trie_iter_walks_subject_then_object_for_fixed_predicate() {
    // Pattern: (?s, 10, ?o) — variable order [s, o].
    let src = source();
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let var_order = vec![Var(0), Var(1)];
    let mut it = PatternTrieIter::new(&src, &pat, &var_order, Ordering::Pso).unwrap();

    // Depth 0 = variable ?s.
    assert_eq!(it.peek(0), Some(1));
    it.open_level(0);
    // Depth 1 = variable ?o, under s=1.
    assert_eq!(it.peek(1), Some(100));
    it.seek(1, 150);
    assert_eq!(it.peek(1), Some(200));
    it.up(1);
    it.seek(0, 2);
    assert_eq!(it.peek(0), Some(2));
    it.open_level(0);
    assert_eq!(it.peek(1), Some(100));
    it.seek(1, 200);
    assert_eq!(it.peek(1), Some(400));
}

#[test]
fn pattern_trie_iter_filters_out_non_matching_predicate() {
    let src = source();
    let pat = TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Var(Var(1)));
    let var_order = vec![Var(0), Var(1)];
    let it = PatternTrieIter::new(&src, &pat, &var_order, Ordering::Pso).unwrap();
    assert_eq!(it.peek(0), None);
}
