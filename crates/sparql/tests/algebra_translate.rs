use horndb_sparql::algebra::{translate, Algebra};
use horndb_sparql::parser::{parse_query, ParsedQuery};

fn alg_of(query: &str) -> Algebra {
    let q = parse_query(query).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe not supported here"),
    };
    translate::translate_query(&inner).expect("translate")
}

#[test]
fn select_one_pattern_is_project_over_bgp() {
    let alg = alg_of("SELECT ?s WHERE { ?s <http://ex/p> ?o }");
    match alg {
        Algebra::Project { vars, inner } => {
            assert_eq!(vars.len(), 1);
            assert_eq!(vars[0].name(), "s");
            assert!(matches!(*inner, Algebra::Bgp { .. }));
        }
        other => panic!("expected Project, got {other:?}"),
    }
}

#[test]
fn ask_is_project_zero_vars() {
    // ASK queries reduce to a "does the BGP produce any row" check,
    // which we represent as a Project with no vars wrapped around the
    // pattern. The runtime turns this into a boolean.
    let alg = alg_of("ASK { ?s ?p ?o }");
    match alg {
        Algebra::Project { vars, .. } => assert!(vars.is_empty()),
        other => panic!("expected Project, got {other:?}"),
    }
}

#[test]
fn join_of_two_bgps() {
    let alg = alg_of("SELECT * WHERE { ?s <http://ex/p> ?o . ?o <http://ex/q> ?z }");
    // Two patterns over distinct predicates land in a single BGP node
    // (spargebra merges them); we just verify the BGP carries both.
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    match inner {
        Algebra::Bgp { patterns } => assert_eq!(patterns.len(), 2),
        other => panic!("expected Bgp, got {other:?}"),
    }
}

#[test]
fn rejects_minus() {
    let q =
        parse_query("SELECT * WHERE { ?s ?p ?o MINUS { ?s <http://ex/q> ?z } }").expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let err = translate::translate_query(&inner).unwrap_err();
    assert!(format!("{err}").contains("Minus"), "got: {err}");
}

#[test]
fn rejects_kleene_star_path() {
    let q = parse_query("SELECT ?x WHERE { ?x <http://ex/p>* ?y }").expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let err = translate::translate_query(&inner).unwrap_err();
    assert!(format!("{err}").contains("property-path"), "got: {err}");
}
