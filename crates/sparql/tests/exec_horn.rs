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
fn user_variable_resembling_alias_does_not_collide() {
    let mut st = HornBackend::new();
    st.insert_triple(iri("a"), iri("likes"), iri("a"));
    st.insert_triple(iri("a"), iri("likes"), iri("b"));
    // ?x repeats within the first pattern (diagonal); the second pattern
    // binds a user variable spelled like the OLD alias scheme.
    let patterns = vec![
        pat(var("x"), iri("likes"), var("x")),
        pat(var("x"), iri("likes"), var("__horndb_dup_x_2")),
    ];
    let rows: Vec<_> = st.scan_bgp(&patterns).unwrap().collect();
    // Diagonal pins ?x = a; ?__horndb_dup_x_2 ranges over {a, b}.
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.get("x"), Some(&iri("a")));
        assert!(r.get("__horndb_dup_x_2").is_some(), "user var must survive");
    }
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

/// #67 consequence-2 regression: a multi-pattern query over a
/// six-figure store must complete in test-grade time (the Stage-1
/// MemStore nested loop needed >20 s at this scale). Debug-build
/// timings are noisy, so the bound is generous; the point is
/// "seconds, not minutes".
///
/// The load uses `insert_algebra_triples_bulk` (one `insert_triples`
/// call per predicate group) rather than per-triple inserts to avoid
/// the O(n²) columnar-rebuild cost that makes the Stage-1 MemStore
/// insert path slow at this scale.
#[test]
fn six_figure_store_multi_pattern_smoke() {
    let mut st = HornBackend::new();
    let n: usize = 100_000;
    let mut triples: Vec<(Term, Term, Term)> = Vec::with_capacity(3 * n);
    for i in 0..n {
        let s = iri(&format!("e{i}"));
        triples.push((s.clone(), iri("a"), iri(&format!("T{}", i % 50))));
        triples.push((
            s.clone(),
            iri("score"),
            Term::Literal(format!(
                "\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>",
                i % 1000
            )),
        ));
        triples.push((s, iri("next"), iri(&format!("e{}", (i + 1) % n))));
    }
    st.insert_algebra_triples_bulk(triples);
    let started = std::time::Instant::now();
    let q = "SELECT ?x ?y WHERE { \
        ?x <http://ex/a> <http://ex/T7> . \
        ?x <http://ex/next> ?y . \
        ?y <http://ex/a> <http://ex/T8> . \
        ?x <http://ex/score> ?s . }";
    match execute_query(q, &st).unwrap() {
        QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 2000),
        other => panic!("expected solutions, got {other:?}"),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "query took {:?}",
        started.elapsed()
    );
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

#[cfg(feature = "reasoner")]
#[test]
fn literal_with_quotes_and_backslashes_survives_reasoner_round_trip() {
    use oxrdf::{Dataset, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};
    let raw = "a \"quoted\" \\ value";
    let mut dataset = Dataset::default();
    dataset.insert(&Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/x").unwrap()),
        NamedNode::new("http://ex/p").unwrap(),
        Literal::new_simple_literal(raw),
        GraphName::DefaultGraph,
    ));
    let mut backend = HornBackend::new();
    horndb_sparql::exec::horn::load_with_reasoning(&mut backend, &dataset).unwrap();
    // NB: the local `iri` helper prepends "http://ex/".
    let patterns = vec![pat(iri("x"), iri("p"), var("v"))];
    let rows: Vec<_> = backend.scan_bgp(&patterns).unwrap().collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("v"),
        Some(&Term::Literal(Literal::new_simple_literal(raw).to_string())),
        "engine-key literal must round-trip with correct N-Triples escaping on the algebra side"
    );
}
