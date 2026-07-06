use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

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
    let ans = execute_query("CONSTRUCT { ?s <http://ex/r> ?o } WHERE { ?s ?p ?o }", &s).unwrap();
    match ans {
        QueryAnswer::Triples(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].1, "http://ex/r");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn plan_select_routes_only_plain_select() {
    use horndb_sparql::api::plan_select;
    use horndb_sparql::SparqlConfig;

    let cfg = SparqlConfig::default();
    let (vars, _plan) = plan_select("SELECT ?s ?o WHERE { ?s ?p ?o }", &cfg)
        .unwrap()
        .expect("a plain SELECT plans for streaming");
    assert_eq!(vars, vec!["s".to_string(), "o".to_string()]);

    for q in [
        "ASK { ?s ?p ?o }",
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        "DESCRIBE <http://ex/a>",
        "EXPLAIN SELECT ?s WHERE { ?s ?p ?o }",
    ] {
        assert!(
            plan_select(q, &cfg).unwrap().is_none(),
            "{q} must fall back to execute_query"
        );
    }

    assert!(plan_select("SELECT ?s WHERE { ?s", &cfg).is_err());
}
