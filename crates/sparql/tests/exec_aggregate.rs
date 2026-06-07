//! GROUP BY + aggregate evaluation tests.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    // alice knows bob, carol; bob knows dave. 3 knows-triples.
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

fn solutions(q: &str, store: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, store).expect("query") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Read the lexical value out of a bound literal/iri term.
fn val(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::BlankNode(s) => s.clone(),
        Term::Literal(raw) => {
            // strip the surrounding quotes / datatype for the simple
            // `"42"^^<...>` shape used by integer aggregates.
            let raw = raw.trim();
            if let Some(rest) = raw.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    return rest[..end].to_owned();
                }
            }
            raw.to_owned()
        }
        other => panic!("unexpected term {other:?}"),
    }
}

#[test]
fn implicit_group_count_star() {
    let s = make_store();
    let rows = solutions("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }", &s);
    assert_eq!(rows.len(), 1, "implicit group yields exactly one row");
    assert_eq!(val(rows[0].get("c").unwrap()), "3");
}

#[test]
fn implicit_group_count_star_zero_when_no_match() {
    let s = make_store();
    let rows = solutions(
        "SELECT (COUNT(*) AS ?c) WHERE { ?s <http://ex/nope> ?o }",
        &s,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(val(rows[0].get("c").unwrap()), "0");
}

#[test]
fn count_var() {
    let s = make_store();
    let rows = solutions(
        "SELECT (COUNT(?o) AS ?c) WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "3");
}

#[test]
fn count_distinct() {
    let s = make_store();
    // Distinct subjects: alice, bob -> 2.
    let rows = solutions(
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "2");
}

#[test]
fn group_by_subject_count() {
    let s = make_store();
    let rows = solutions(
        "SELECT ?s (COUNT(?o) AS ?c) WHERE { ?s <http://ex/knows> ?o } GROUP BY ?s",
        &s,
    );
    assert_eq!(rows.len(), 2, "two distinct subjects");
    let mut got: Vec<(String, String)> = rows
        .iter()
        .map(|r| (val(r.get("s").unwrap()), val(r.get("c").unwrap())))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("http://ex/alice".to_string(), "2".to_string()),
            ("http://ex/bob".to_string(), "1".to_string()),
        ]
    );
}
