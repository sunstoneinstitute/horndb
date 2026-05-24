use reasoner_sparql::algebra::translate::translate_query;
use reasoner_sparql::algebra::Term;
use reasoner_sparql::exec::mem::MemStore;
use reasoner_sparql::exec::runtime::construct_triples;
use reasoner_sparql::exec::runtime::Runtime;
use reasoner_sparql::exec::Store;
use reasoner_sparql::parser::{parse_query, ParsedQuery};
use reasoner_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

#[test]
fn construct_rewrites_pairs() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_triple(iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));

    let q = parse_query("CONSTRUCT { ?s <http://ex/related> ?o } WHERE { ?s <http://ex/p> ?o }")
        .unwrap();
    let inner = match q {
        ParsedQuery::Construct { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let rows: Vec<_> = Runtime::new(&s).run(&plan).unwrap().collect();
    let triples = construct_triples(&inner, &rows).unwrap();
    assert_eq!(triples.len(), 2);
    assert!(triples
        .iter()
        .any(|(s, p, o)| s == "http://ex/a" && p == "http://ex/related" && o == "http://ex/b"));
}
