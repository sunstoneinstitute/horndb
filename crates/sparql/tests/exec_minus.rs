//! `MINUS` evaluation (SPARQL 1.1 §18.5, HDB-133). The W3C `negation/` cases
//! cover this end to end, but they need the fetched corpus; these run in the
//! plain `cargo nextest` pass and pin the two branches of the anti-join:
//! shared variables (exclude compatible rows) and no shared variables at all
//! (exclude nothing).

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const EX: &str = "http://example.org/";

fn iri(local: &str) -> Term {
    Term::Iri(format!("{EX}{local}"))
}

/// `:a :p :x`, `:b :p :y`, and `:a :flag :on` — so `?s` in {a, b} and only
/// `a` carries the flag. `:z :other :w` is the disjoint-variable fixture.
fn store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("a"), iri("p"), iri("x"));
    s.insert_triple(iri("b"), iri("p"), iri("y"));
    s.insert_triple(iri("a"), iri("flag"), iri("on"));
    s.insert_triple(iri("z"), iri("other"), iri("w"));
    s
}

fn subjects(q: &str) -> Vec<String> {
    let answer = execute_query(q, &store()).expect("query should run");
    let QueryAnswer::Solutions { rows, .. } = answer else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .map(|b| match b.get("s").expect("?s bound") {
            Term::Iri(s) => s.trim_start_matches(EX).to_owned(),
            other => panic!("unexpected term {other:?}"),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn minus_excludes_rows_compatible_on_a_shared_variable() {
    let q = format!("SELECT ?s WHERE {{ ?s <{EX}p> ?o MINUS {{ ?s <{EX}flag> <{EX}on> }} }}");
    assert_eq!(subjects(&q), vec!["b"]);
}

#[test]
fn minus_with_no_shared_variable_excludes_nothing() {
    // §18.5's domain-intersection rule: the right pattern matches (it binds
    // ?t), but shares no variable with the left, so every left row survives.
    // A naive "compatible => drop" anti-join would return nothing here.
    let q = format!("SELECT ?s WHERE {{ ?s <{EX}p> ?o MINUS {{ ?t <{EX}other> <{EX}w> }} }}");
    assert_eq!(subjects(&q), vec!["a", "b"]);
}
