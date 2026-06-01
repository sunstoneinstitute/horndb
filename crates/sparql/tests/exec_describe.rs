//! DESCRIBE query-form integration tests.
//!
//! Exercises the full pipeline via `execute_query` returning
//! `QueryAnswer::Triples` (forward, one-level Concise Bounded
//! Description). Mirrors the conventions in `exec_construct.rs`.

use horndb_sparql::algebra::{Term, TriplePattern};
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::describe_triples;
use horndb_sparql::exec::{Bindings, Executor, Store};
use horndb_sparql::Result;

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
fn describe_explicit_iri_with_empty_where_still_describes() {
    // SPARQL 1.1 §16.4: an IRI named directly in the DESCRIBE clause is
    // described in addition to the WHERE solutions. Here the WHERE matches
    // nothing, so the only resource to describe is the explicit <a>.
    // Regression: previously the explicit IRI was lowered to an Extend over
    // the (empty) WHERE rows and was therefore dropped, returning an empty
    // graph.
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));

    let ans = execute_query(
        "DESCRIBE <http://ex/a> WHERE { <http://ex/missing> ?p ?o }",
        &s,
    )
    .unwrap();
    let t = triples(ans);
    assert_eq!(t.len(), 1, "explicit IRI must still be described: {t:?}");
    assert!(
        t.iter()
            .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/b"),
        "expected (<a>, <p>, <b>): {t:?}"
    );
}

#[test]
fn describe_explicit_iri_with_limit_and_empty_where() {
    // Regression (#48): solution modifiers (here LIMIT) nest extra unary
    // nodes between the top Project and the seed Extend chain, so the old
    // "peel Extends directly under Project" logic saw a `Slice` and stopped,
    // dropping the explicit IRI. The full modifier-spine walk must still seed
    // <a> even though the WHERE matches nothing.
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));

    let ans = execute_query(
        "DESCRIBE <http://ex/a> WHERE { <http://ex/missing> ?p ?o } LIMIT 10",
        &s,
    )
    .unwrap();
    let t = triples(ans);
    assert!(
        t.iter()
            .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/b"),
        "expected (<a>, <p>, <b>) through LIMIT modifier: {t:?}"
    );
}

#[test]
fn describe_explicit_iri_with_order_by_and_empty_where() {
    // Regression (#48): ORDER BY / OFFSET / LIMIT together nest several unary
    // modifier nodes above the seed Extend chain. The explicit IRI must still
    // be described when the WHERE produces no rows.
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));

    let ans = execute_query(
        "DESCRIBE <http://ex/a> WHERE { <http://ex/missing> ?p ?o } ORDER BY ?p OFFSET 1 LIMIT 5",
        &s,
    )
    .unwrap();
    let t = triples(ans);
    assert!(
        t.iter()
            .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/b"),
        "expected (<a>, <p>, <b>) through ORDER BY/OFFSET/LIMIT modifiers: {t:?}"
    );
}

#[test]
fn describe_multiple_explicit_iris_with_matching_where() {
    // Two explicit IRIs in the DESCRIBE clause, plus a WHERE that matches
    // (so there is at least one solution row). `DESCRIBE <a> <b>` names no
    // variables, so spargebra projects only the two fresh BIND vars
    // carrying <a> and <b>; the WHERE's own variables (?s/?o) are projected
    // away and are not themselves describe targets. Both explicit IRIs must
    // be described.
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/x"));
    s.insert_triple(iri("http://ex/b"), iri("http://ex/q"), iri("http://ex/y"));
    // A triple the WHERE matches, ensuring the solution set is non-empty.
    s.insert_triple(iri("http://ex/c"), iri("http://ex/r"), iri("http://ex/z"));

    let ans = execute_query(
        "DESCRIBE <http://ex/a> <http://ex/b> WHERE { ?s <http://ex/r> ?o }",
        &s,
    )
    .unwrap();
    let t = triples(ans);
    assert_eq!(t.len(), 2, "both explicit IRIs described: {t:?}");
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/a" && p == "http://ex/p" && o == "http://ex/x"));
    assert!(t
        .iter()
        .any(|(sub, p, o)| sub == "http://ex/b" && p == "http://ex/q" && o == "http://ex/y"));
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

/// A **kind-aware** mock executor. Unlike `MemStore` (which erases term
/// kinds by binding every scanned value as `Term::Iri`), this backend
/// only unifies a constant-subject pattern when BOTH the lexical value
/// AND the term kind match. It supports exactly the one pattern shape
/// `describe_triples` issues: a constant subject with two object/pred
/// variables.
struct KindAwareStore {
    triples: Vec<(Term, Term, Term)>,
}

impl Executor for KindAwareStore {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        assert_eq!(patterns.len(), 1, "mock only handles single-pattern scans");
        let pat = &patterns[0];
        // Expect the describe shape: constant subject, var predicate, var object.
        let (pred_var, obj_var) = match (&pat.predicate, &pat.object) {
            (Term::Var(p), Term::Var(o)) => (p.name().to_owned(), o.name().to_owned()),
            _ => panic!("mock only handles (const, var, var) patterns"),
        };
        let mut out = Vec::new();
        for (s, p, o) in &self.triples {
            // Kind-aware match: the stored subject term must equal the
            // pattern subject term exactly (kind included). A
            // `Term::Iri("b0")` pattern will NOT match a stored
            // `Term::BlankNode("b0")`, and vice versa.
            if *s == pat.subject {
                let mut b = Bindings::new();
                b.set(pred_var.clone(), p.clone());
                b.set(obj_var.clone(), o.clone());
                out.push(b);
            }
        }
        Ok(Box::new(out.into_iter()))
    }
}

/// Regression: a type-preserving backend can bind a DESCRIBE target to a
/// `Term::BlankNode`. Scanning it as a `Term::Iri` (the old behaviour)
/// would miss its outgoing triple. `describe_triples` must scan with the
/// original term kind so blank-node and IRI targets are both described.
#[test]
fn describe_preserves_target_term_kind() {
    let store = KindAwareStore {
        triples: vec![
            // Subject is a *blank node* "b0".
            (
                Term::BlankNode("b0".into()),
                iri("http://ex/p"),
                iri("http://ex/x"),
            ),
            // A decoy: an IRI "b0" with the SAME lexical form but a
            // different kind. The blank-node scan must NOT pick this up.
            (iri("b0"), iri("http://ex/decoy"), iri("http://ex/never")),
            // And an ordinary IRI subject, to confirm IRIs still work.
            (iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/y")),
        ],
    };

    // Row binds one var to the blank node, another to the IRI.
    let mut row = Bindings::new();
    row.set("s", Term::BlankNode("b0".into()));
    row.set("t", iri("http://ex/a"));

    let t = describe_triples(&store, &[], &[row]).unwrap();

    // The blank node's outgoing triple is described, with the decoy
    // (same lexical, IRI kind) excluded.
    assert!(
        t.iter()
            .any(|(s, p, o)| s == "b0" && p == "http://ex/p" && o == "http://ex/x"),
        "blank-node target must be scanned as a blank node: {t:?}"
    );
    assert!(
        !t.iter().any(|(_, p, _)| p == "http://ex/decoy"),
        "IRI decoy with same lexical form must not be matched: {t:?}"
    );
    // The IRI target is described too.
    assert!(
        t.iter()
            .any(|(s, p, o)| s == "http://ex/a" && p == "http://ex/q" && o == "http://ex/y"),
        "IRI target must still be described: {t:?}"
    );
    assert_eq!(t.len(), 2, "exactly the blank-node and IRI triples: {t:?}");
}
