use reasoner_sparql::algebra::Term;
use reasoner_sparql::api::{execute_query, QueryAnswer};
use reasoner_sparql::exec::mem::MemStore;
use reasoner_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

#[test]
fn end_to_end_select() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query("SELECT ?o WHERE { ?s ?p ?o }", &s).unwrap();
    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            assert_eq!(vars, vec!["o".to_string()]);
            assert_eq!(rows.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn end_to_end_ask_true() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query("ASK { ?s ?p ?o }", &s).unwrap();
    assert!(matches!(ans, QueryAnswer::Boolean(true)));
}

#[test]
fn end_to_end_construct() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query(
        "CONSTRUCT { ?s <http://ex/r> ?o } WHERE { ?s ?p ?o }",
        &s,
    )
    .unwrap();
    match ans {
        QueryAnswer::Triples(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].1, "http://ex/r");
        }
        other => panic!("unexpected: {other:?}"),
    }
}
