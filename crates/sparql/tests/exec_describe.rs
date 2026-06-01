//! DESCRIBE query-form integration tests.
//!
//! Exercises the full pipeline via `execute_query` returning
//! `QueryAnswer::Triples` (forward, one-level Concise Bounded
//! Description). Mirrors the conventions in `exec_construct.rs`.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn triples(ans: QueryAnswer) -> Vec<(String, String, String)> {
    match ans {
        QueryAnswer::Triples(t) => t,
        other => panic!("expected Triples, got {other:?}"),
    }
}

#[test]
fn describe_iri_no_where_returns_forward_triples() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_triple(iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c"));
    s.insert_triple(
        iri("http://ex/other"),
        iri("http://ex/p"),
        iri("http://ex/z"),
    );

    let ans = execute_query("DESCRIBE <http://ex/a>", &s).unwrap();
    let t = triples(ans);
    assert_eq!(t.len(), 2);
    assert!(t.iter().all(|(sub, _, _)| sub == "http://ex/a"));
    assert!(t
        .iter()
        .any(|(_, p, o)| p == "http://ex/p" && o == "http://ex/b"));
    assert!(t
        .iter()
        .any(|(_, p, o)| p == "http://ex/q" && o == "http://ex/c"));
}

#[test]
fn describe_var_describes_each_bound_resource() {
    let mut s = MemStore::default();
    // Two subjects that match the WHERE clause.
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/x"));
    s.insert_triple(iri("http://ex/b"), iri("http://ex/p"), iri("http://ex/y"));
    // Extra forward triples that DESCRIBE should also emit.
    s.insert_triple(iri("http://ex/a"), iri("http://ex/r"), iri("http://ex/m"));

    let ans = execute_query("DESCRIBE ?s WHERE { ?s <http://ex/p> ?o }", &s).unwrap();
    let t = triples(ans);
    // a: p->x, r->m ; b: p->y  => 3 triples
    assert_eq!(t.len(), 3);
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/x"));
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/r" && o == "http://ex/m"));
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/b" && p == "http://ex/p" && o == "http://ex/y"));
}

#[test]
fn describe_dedupes_and_sorts_deterministically() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/z"), iri("http://ex/3"));
    s.insert_triple(iri("http://ex/a"), iri("http://ex/m"), iri("http://ex/2"));
    s.insert_triple(iri("http://ex/a"), iri("http://ex/a"), iri("http://ex/1"));

    // A single explicit IRI: its forward triples must come back sorted
    // and deduplicated.
    let ans = execute_query("DESCRIBE <http://ex/a>", &s).unwrap();
    let t = triples(ans);
    let mut sorted = t.clone();
    sorted.sort();
    assert_eq!(t, sorted, "output must be in sorted order");
    // No duplicates.
    let mut deduped = t.clone();
    deduped.dedup();
    assert_eq!(t, deduped, "output must be deduplicated");
    assert_eq!(t.len(), 3);
}

#[test]
fn describe_multi_var_dedupes_shared_resource() {
    let mut s = MemStore::default();
    // <b> is both the object of the first triple and the subject of the
    // second, so `DESCRIBE ?s ?o WHERE { ?s ?p ?o }` binds it under both
    // ?s and ?o. Its forward triple must be described exactly once.
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_triple(iri("http://ex/b"), iri("http://ex/p"), iri("http://ex/c"));

    let ans = execute_query("DESCRIBE ?s ?o WHERE { ?s ?p ?o }", &s).unwrap();
    let t = triples(ans);

    // Resources bound: a, b (subjects); b, c (objects) => {a, b, c}.
    // Forward triples: a p b, b p c. c has no forward triples.
    assert_eq!(
        t.len(),
        2,
        "shared resource <b> must not be described twice"
    );
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/b"));
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/b" && p == "http://ex/p" && o == "http://ex/c"));

    // No duplicates.
    let mut deduped = t.clone();
    deduped.dedup();
    assert_eq!(t, deduped, "output must be deduplicated");
}

#[test]
fn describe_non_subject_iri_returns_empty() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    // <http://ex/b> appears only as an object, never a subject.
    let ans = execute_query("DESCRIBE <http://ex/b>", &s).unwrap();
    let t = triples(ans);
    assert!(t.is_empty());
}

#[test]
fn describe_is_stable_across_runs() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_triple(iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c"));

    let first = triples(execute_query("DESCRIBE <http://ex/a>", &s).unwrap());
    for _ in 0..5 {
        let again = triples(execute_query("DESCRIBE <http://ex/a>", &s).unwrap());
        assert_eq!(first, again);
    }
}
