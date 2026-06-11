//! HornBackend executor tests — mirrors the MemStore scenarios in
//! `mem.rs` plus the #67-specific behaviors (term typing, WCOJ routing,
//! ground patterns, repeated variables).

use horndb_sparql::algebra::{Term, TriplePattern, Var};
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{Executor, Store};

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}
fn lit(s: &str) -> Term {
    Term::Literal(format!("\"{s}\""))
}
fn var(s: &str) -> Term {
    Term::Var(Var::new(s))
}
fn pat(s: Term, p: Term, o: Term) -> TriplePattern {
    TriplePattern {
        subject: s,
        predicate: p,
        object: o,
    }
}

fn store() -> HornBackend {
    let mut st = HornBackend::new();
    for (s, p, o) in [
        ("cw1", "a", "BlogPost"),
        ("cw2", "a", "BlogPost"),
        ("cw3", "a", "NewsItem"),
    ] {
        st.insert_triple(iri(s), iri(p), iri(o));
    }
    st.insert_triple(iri("cw1"), iri("title"), lit("First"));
    st.insert_triple(iri("cw1"), iri("body"), lit("Hello"));
    st.insert_triple(iri("cw2"), iri("title"), lit("Second"));
    st.insert_triple(iri("cw3"), iri("title"), lit("Third"));
    st
}

#[test]
fn two_pattern_join_binds_kind_correct_terms() {
    let st = store();
    let patterns = vec![
        pat(var("cw"), iri("a"), iri("BlogPost")),
        pat(var("cw"), iri("title"), var("t")),
    ];
    let mut rows: Vec<(Term, Term)> = st
        .scan_bgp(&patterns)
        .unwrap()
        .map(|b| (b.get("cw").unwrap().clone(), b.get("t").unwrap().clone()))
        .collect();
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![(iri("cw1"), lit("First")), (iri("cw2"), lit("Second")),],
        "literals must come back as Term::Literal, not Term::Iri"
    );
}

#[test]
fn four_pattern_bgp_takes_wcoj_path() {
    // >= 4 patterns crosses Planner::default()'s WCOJ cutover.
    let st = store();
    let patterns = vec![
        pat(var("cw"), iri("a"), iri("BlogPost")),
        pat(var("cw"), iri("title"), var("t")),
        pat(var("cw"), iri("body"), var("b")),
        pat(var("cw2"), iri("a"), iri("BlogPost")),
    ];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    // cw1 x {cw1, cw2}: only cw1 has a body.
    assert_eq!(rows.len(), 2);
}

#[test]
fn ground_pattern_filters_without_executor() {
    let st = store();
    // Present ground triple + one var pattern.
    let patterns = vec![
        pat(iri("cw1"), iri("a"), iri("BlogPost")),
        pat(var("x"), iri("a"), iri("NewsItem")),
    ];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    // Absent ground triple zeroes the result.
    let patterns = vec![
        pat(iri("cw1"), iri("a"), iri("NewsItem")),
        pat(var("x"), iri("a"), iri("BlogPost")),
    ];
    assert_eq!(st.scan_bgp(&patterns).unwrap().count(), 0);
    // All-ground, all-present: exactly one empty row (ASK semantics).
    let patterns = vec![pat(iri("cw1"), iri("a"), iri("BlogPost"))];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_empty());
}

#[test]
fn repeated_variable_within_pattern_filters_to_diagonal() {
    let mut st = HornBackend::new();
    st.insert_triple(iri("a"), iri("likes"), iri("a"));
    st.insert_triple(iri("a"), iri("likes"), iri("b"));
    let patterns = vec![pat(var("x"), iri("likes"), var("x"))];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("x"), Some(&iri("a")));
}

#[test]
fn unknown_constant_yields_empty_not_error() {
    let st = store();
    let patterns = vec![pat(var("x"), iri("never-seen"), var("y"))];
    assert_eq!(st.scan_bgp(&patterns).unwrap().count(), 0);
}

#[test]
fn order_by_literal_object_uses_value_semantics() {
    // The #67 consequence-3 regression: typed literals must survive the
    // dictionary with their kind, so ORDER BY compares values.
    let mut st = HornBackend::new();
    for (s, n) in [("x1", "10"), ("x2", "2"), ("x3", "30")] {
        st.insert_triple(
            iri(s),
            iri("count"),
            Term::Literal(format!(
                "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            )),
        );
    }
    let q = "SELECT ?s WHERE { ?s <http://ex/count> ?n } ORDER BY ?n";
    match execute_query(q, &st).unwrap() {
        QueryAnswer::Solutions { rows, .. } => {
            let order: Vec<_> = rows.iter().map(|r| r.get("s").unwrap().clone()).collect();
            assert_eq!(order, vec![iri("x2"), iri("x1"), iri("x3")]);
        }
        other => panic!("expected solutions, got {other:?}"),
    }
}

#[test]
fn empty_pattern_list_yields_single_empty_row() {
    let st = HornBackend::new();
    let rows: Vec<_> = st.scan_bgp(&[]).unwrap().collect();
    assert_eq!(rows.len(), 1);
}

#[cfg(feature = "reasoner")]
#[test]
fn materialized_closure_is_queryable() {
    use oxrdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad};
    let nn = |s: &str| NamedNode::new(s).unwrap();
    let nb = |s: &str| NamedOrBlankNode::NamedNode(nn(s));
    let mut dataset = Dataset::default();
    // :Penguin rdfs:subClassOf :Bird . :pingu a :Penguin .
    dataset.insert(&Quad::new(
        nb("http://ex/Penguin"),
        nn("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
        nn("http://ex/Bird"),
        GraphName::DefaultGraph,
    ));
    dataset.insert(&Quad::new(
        nb("http://ex/pingu"),
        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        nn("http://ex/Penguin"),
        GraphName::DefaultGraph,
    ));
    let mut backend = HornBackend::new();
    let stats = horndb_sparql::exec::horn::load_with_reasoning(&mut backend, &dataset).unwrap();
    assert!(stats.loaded >= 2);
    // cax-sco: pingu must now be a Bird, visible through SPARQL.
    let q = "ASK { <http://ex/pingu> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Bird> }";
    match execute_query(q, &backend).unwrap() {
        QueryAnswer::Boolean(b) => assert!(b, "inferred triple must be queryable"),
        other => panic!("expected boolean, got {other:?}"),
    }
}
