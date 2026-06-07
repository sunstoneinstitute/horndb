//! FILTER comparison (<=, >=) and membership (IN, NOT IN) tests.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}
fn int(n: i64) -> Term {
    Term::Literal(format!(
        "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    ))
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    // three items with integer ages 10, 20, 30.
    for (item, age) in [("a", 10), ("b", 20), ("c", 30)] {
        s.insert_triple(
            iri(&format!("http://ex/{item}")),
            iri("http://ex/age"),
            int(age),
        );
    }
    s
}

fn subjects(q: &str, store: &MemStore) -> Vec<String> {
    let rows = match execute_query(q, store).expect("query") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    };
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("s") {
            Some(Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

#[test]
fn filter_le() {
    let s = make_store();
    let got = subjects(
        "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age <= 20) }",
        &s,
    );
    assert_eq!(
        got,
        vec!["http://ex/a".to_string(), "http://ex/b".to_string()]
    );
}

#[test]
fn filter_ge() {
    let s = make_store();
    let got = subjects(
        "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age >= 20) }",
        &s,
    );
    assert_eq!(
        got,
        vec!["http://ex/b".to_string(), "http://ex/c".to_string()]
    );
}

#[test]
fn filter_in() {
    let s = make_store();
    let got = subjects(
        "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age IN (10, 30)) }",
        &s,
    );
    assert_eq!(
        got,
        vec!["http://ex/a".to_string(), "http://ex/c".to_string()]
    );
}

#[test]
fn filter_not_in() {
    let s = make_store();
    let got = subjects(
        "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age NOT IN (10, 30)) }",
        &s,
    );
    assert_eq!(got, vec!["http://ex/b".to_string()]);
}

#[test]
fn filter_numeric_range_not_lexical() {
    // Lexical comparison would rank "9" > "10"; numeric coercion must
    // keep 9 < 10 so this item passes the upper bound.
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/x"), iri("http://ex/age"), int(9));
    let got = subjects(
        "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age <= 10) }",
        &s,
    );
    assert_eq!(got, vec!["http://ex/x".to_string()]);
}
