//! End-to-end tests for the expanded expression surface (#66):
//! arithmetic, IF, COALESCE, builtin functions.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn store_with_prices() -> MemStore {
    let mut s = MemStore::default();
    for (subj, price) in [("a", 4), ("b", 11)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{subj}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    s
}

fn rows(q: &str, s: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, s).expect("query should run") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Lexical value of a binding, ignoring term kind and literal decoration.
fn lexical(b: &horndb_sparql::exec::Bindings, var: &str) -> String {
    let t = b.get(var).unwrap_or_else(|| panic!("unbound ?{var}"));
    let raw = match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        other => panic!("unexpected term {other:?}"),
    };
    if let Some(stripped) = raw.strip_prefix('"') {
        stripped.split('"').next().unwrap().to_owned()
    } else {
        raw
    }
}

#[test]
fn bind_arithmetic_add() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?y WHERE { ?s <http://example.org/price> ?p . BIND(?p + 1 AS ?y) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "y")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "5".into()),
            ("http://example.org/b".into(), "12".into()),
        ]
    );
}

#[test]
fn filter_arithmetic_comparison() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s WHERE { ?s <http://example.org/price> ?p . FILTER(?p * 2 > 10) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/b");
}

#[test]
fn division_yields_decimal_and_div_by_zero_drops_row() {
    let s = store_with_prices();
    // 4 / 2 = 2 ; 11 / 2 = 5.5 — both rows keep a bound ?h.
    let got = rows(
        "SELECT ?h WHERE { ?s <http://example.org/price> ?p . BIND(?p / 2 AS ?h) }",
        &s,
    );
    let mut vals: Vec<String> = got.iter().map(|b| lexical(b, "h")).collect();
    vals.sort();
    assert_eq!(vals, vec!["2".to_string(), "5.5".to_string()]);
    // Division by zero is an expression error: BIND leaves ?z unbound.
    let got = rows(
        "SELECT ?s ?z WHERE { ?s <http://example.org/price> ?p . BIND(?p / 0 AS ?z) }",
        &s,
    );
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|b| b.get("z").is_none()));
}

#[test]
fn unary_minus() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?n WHERE { ?s <http://example.org/price> ?p . BIND(-?p AS ?n) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "n"), "-4");
}

#[test]
fn if_in_bind() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?label WHERE { ?s <http://example.org/price> ?p . \
         BIND(IF(?p > 10, \"expensive\", \"cheap\") AS ?label) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "label")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "cheap".into()),
            ("http://example.org/b".into(), "expensive".into()),
        ]
    );
}

#[test]
fn coalesce_picks_first_bound() {
    let s = store_with_prices();
    // ?unbound never binds; COALESCE falls through to ?p.
    let got = rows(
        "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
         OPTIONAL { ?s <http://example.org/missing> ?unbound } \
         BIND(COALESCE(?unbound, ?p) AS ?v) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "v"), "4");
}

#[test]
fn sum_of_products_aggregate() {
    let mut s = MemStore::default();
    for (o, qty, price) in [("o1", 2, 3), ("o2", 5, 4)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/qty".into()),
            Term::Literal(format!("\"{qty}\"^^<{XSD_INT}>")),
        );
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    let got = rows(
        "SELECT (SUM(?q * ?p) AS ?total) WHERE { \
         ?o <http://example.org/qty> ?q . ?o <http://example.org/price> ?p }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "total"), "26");
}
