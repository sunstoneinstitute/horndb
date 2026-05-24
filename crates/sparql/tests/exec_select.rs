use reasoner_sparql::algebra::translate::translate_query;
use reasoner_sparql::algebra::Term;
use reasoner_sparql::exec::mem::MemStore;
use reasoner_sparql::exec::runtime::Runtime;
use reasoner_sparql::exec::Store;
use reasoner_sparql::parser::{parse_query, ParsedQuery};
use reasoner_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/knows"),
        iri("http://ex/bob"),
    );
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/knows"),
        iri("http://ex/carol"),
    );
    s.insert_triple(
        iri("http://ex/bob"),
        iri("http://ex/knows"),
        iri("http://ex/dave"),
    );
    s
}

fn run(q: &str, store: &MemStore) -> Vec<reasoner_sparql::exec::Bindings> {
    let inner = match parse_query(q).unwrap() {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe"),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    Runtime::new(store).run(&plan).unwrap().collect()
}

#[test]
fn select_star_returns_all_subjects() {
    let s = make_store();
    let rows = run("SELECT ?s WHERE { ?s <http://ex/knows> ?o }", &s);
    let mut subjs: Vec<String> = rows
        .iter()
        .map(|b| match b.get("s").unwrap() {
            Term::Iri(s) => s.clone(),
            _ => panic!(),
        })
        .collect();
    subjs.sort();
    subjs.dedup();
    assert_eq!(
        subjs,
        vec!["http://ex/alice".to_string(), "http://ex/bob".to_string()]
    );
}

#[test]
fn select_distinct_dedups() {
    let s = make_store();
    let rows = run("SELECT DISTINCT ?s WHERE { ?s <http://ex/knows> ?o }", &s);
    assert_eq!(rows.len(), 2);
}

#[test]
fn select_filter_eq() {
    let s = make_store();
    let rows = run(
        r#"SELECT ?o WHERE { ?s <http://ex/knows> ?o . FILTER(?s = <http://ex/alice>) }"#,
        &s,
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn select_limit_offset() {
    let s = make_store();
    let rows = run("SELECT ?o WHERE { ?s <http://ex/knows> ?o } LIMIT 2", &s);
    assert_eq!(rows.len(), 2);
}
