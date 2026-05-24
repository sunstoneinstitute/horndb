use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};

#[test]
fn build_a_bgp() {
    let tp = TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    };
    let alg = Algebra::Bgp { patterns: vec![tp] };
    match alg {
        Algebra::Bgp { patterns } => assert_eq!(patterns.len(), 1),
        other => panic!("expected Bgp, got {other:?}"),
    }
}
