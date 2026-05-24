use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s
}

#[test]
fn ask_true_when_pattern_matches() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s ?p ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(any);
}

#[test]
fn ask_false_when_pattern_misses() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s <http://ex/missing> ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(!any);
}
