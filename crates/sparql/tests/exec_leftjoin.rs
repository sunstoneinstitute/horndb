//! Regression tests for `OPTIONAL` (`PhysicalPlan::LeftJoin`) semantics.
//!
//! These pin the observable behaviour of the left-join across the
//! hash-join rewrite of #116 (the quadratic nested loop that made
//! trainmarks q4 time out at scale). They exercise: matched rows being
//! extended, unmatched left rows being preserved, correct pairing when
//! many distinct join keys are present (guards against hash-bucket
//! cross-contamination), and the inner-`FILTER` (`expr`) arm.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://example.org/{s}"))
}

fn rows(q: &str, s: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, s).expect("query should run") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

fn lexical(b: &horndb_sparql::exec::Bindings, var: &str) -> String {
    let t = b.get(var).unwrap_or_else(|| panic!("unbound ?{var}"));
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s
            .strip_prefix('"')
            .map(|stripped| stripped.split('"').next().unwrap().to_owned())
            .unwrap_or_else(|| s.clone()),
        other => panic!("unexpected term {other:?}"),
    }
}

#[test]
fn optional_matched_extends_unmatched_preserves() {
    let mut s = MemStore::default();
    // Three items; only `a` and `c` carry a label.
    for item in ["a", "b", "c"] {
        s.insert_triple(iri(item), iri("type"), iri("Item"));
    }
    s.insert_triple(iri("a"), iri("label"), Term::Literal("\"Apple\"".into()));
    s.insert_triple(iri("c"), iri("label"), Term::Literal("\"Cherry\"".into()));

    let got = rows(
        "SELECT ?s ?label WHERE { \
         ?s <http://example.org/type> <http://example.org/Item> . \
         OPTIONAL { ?s <http://example.org/label> ?label } }",
        &s,
    );

    // All three items survive (left-join preserves the non-matching row).
    assert_eq!(got.len(), 3);
    let mut pairs: Vec<(String, Option<String>)> = got
        .iter()
        .map(|b| (lexical(b, "s"), b.get("label").map(|_| lexical(b, "label"))))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), Some("Apple".into())),
            ("http://example.org/b".into(), None),
            ("http://example.org/c".into(), Some("Cherry".into())),
        ]
    );
}

#[test]
fn optional_pairs_each_key_with_its_own_value() {
    // Many distinct join keys: each order pairs with exactly its own
    // customer. A miskeyed hash join would cross-contaminate buckets or
    // change the cardinality; the nested loop got this right and so must
    // the rewrite.
    let mut s = MemStore::default();
    const N: usize = 500;
    for i in 0..N {
        let order = format!("order{i}");
        s.insert_triple(iri(&order), iri("type"), iri("Order"));
        s.insert_triple(iri(&order), iri("placedBy"), iri(&format!("customer{i}")));
    }

    let got = rows(
        "SELECT ?o ?c WHERE { \
         ?o <http://example.org/type> <http://example.org/Order> . \
         OPTIONAL { ?o <http://example.org/placedBy> ?c } }",
        &s,
    );

    assert_eq!(got.len(), N);
    for b in &got {
        let o = lexical(b, "o");
        let c = lexical(b, "c");
        let i = o.strip_prefix("http://example.org/order").unwrap();
        assert_eq!(c, format!("http://example.org/customer{i}"));
    }
}

#[test]
#[ignore = "scaling check — run with --release to see the #116 cliff is gone"]
fn optional_scales_without_quadratic_cliff() {
    // The old nested-loop LeftJoin was O(|left|·|right|): at N here it
    // would do ~N² extend_compat+clone iterations and take many seconds.
    // The hash join keeps it ~linear and completes in well under a second.
    let mut s = MemStore::default();
    const N: usize = 20_000;
    for i in 0..N {
        let order = format!("order{i}");
        s.insert_triple(iri(&order), iri("type"), iri("Order"));
        s.insert_triple(iri(&order), iri("placedBy"), iri(&format!("cust{i}")));
    }
    let start = std::time::Instant::now();
    let got = rows(
        "SELECT ?o ?c WHERE { \
         ?o <http://example.org/type> <http://example.org/Order> . \
         OPTIONAL { ?o <http://example.org/placedBy> ?c } }",
        &s,
    );
    let elapsed = start.elapsed();
    assert_eq!(got.len(), N);
    eprintln!("OPTIONAL left-join over {N} rows took {elapsed:?}");
    assert!(
        elapsed.as_secs() < 5,
        "left-join took {elapsed:?} — quadratic regression?"
    );
}

#[test]
fn optional_inner_filter_keeps_left_when_condition_fails() {
    // The OPTIONAL carries a FILTER (the LeftJoin `expr` arm): only the
    // expensive price binds; the cheap row keeps ?p unbound but survives.
    let mut s = MemStore::default();
    for (item, price) in [("a", 4), ("b", 11)] {
        s.insert_triple(iri(item), iri("type"), iri("Item"));
        s.insert_triple(
            iri(item),
            iri("price"),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }

    let got = rows(
        "SELECT ?s ?p WHERE { \
         ?s <http://example.org/type> <http://example.org/Item> . \
         OPTIONAL { ?s <http://example.org/price> ?p . FILTER(?p > 10) } }",
        &s,
    );

    assert_eq!(got.len(), 2);
    let mut pairs: Vec<(String, Option<String>)> = got
        .iter()
        .map(|b| (lexical(b, "s"), b.get("p").map(|_| lexical(b, "p"))))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), None),
            ("http://example.org/b".into(), Some("11".into())),
        ]
    );
}
