use horndb_sparql::algebra::{translate, Algebra, Term, TriplePattern};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::SparqlConfig;

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

// RDF 1.2: SPARQL 1.2 triple-term patterns (`<< s p o >>` / `<<( s p o )>>`)
// are accepted by spargebra under the `sparql-12` feature. The translator
// gates them at runtime on `SparqlConfig::rdf12` so the default (1.1)
// rejects them, and `SparqlConfig::rdf12()` accepts them.

fn parse_select(query: &str) -> spargebra::Query {
    let q = parse_query(query).expect("parse");
    match q {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe not supported here"),
    }
}

#[test]
fn rejects_triple_term_pattern_in_default_mode() {
    // RDF 1.2 triple-term syntax `<<( s p o )>>` (the non-asserting form;
    // `<< s p o >>` is reified-triple syntax which desugars to extra
    // triples even in 1.1 mode). With the default SparqlConfig
    // (rdf12 == false) the translator must refuse to lower the pattern;
    // the SPARQL 1.1 caller stays 1.1.
    let q = parse_select(
        "SELECT ?s WHERE { ?s <http://ex/claims> <<( <http://ex/Bob> <http://ex/age> 30 )>> }",
    );
    let err = translate::translate_query(&q).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("triple-term") || msg.contains("rdf12"),
        "expected triple-term error, got: {msg}"
    );
}

#[test]
fn rdf12_query_executes_against_memstore() {
    // End-to-end smoke: with SparqlConfig::rdf12 the high-level
    // execute_query_with pipeline plans and runs against a MemStore.
    // The MemStore has no triple-term carriage at Stage 1, so the
    // query returns zero rows — but it must NOT error. This guards
    // against accidental "rejected at plan time" regressions.
    use horndb_sparql::api::execute_query_with;
    use horndb_sparql::api::QueryAnswer;
    use horndb_sparql::exec::mem::MemStore;
    let store = MemStore::default();
    let query =
        "SELECT ?s WHERE { ?s <http://ex/claims> <<( <http://ex/Bob> <http://ex/age> 30 )>> }";
    let ans = execute_query_with(query, &store, &SparqlConfig::rdf12()).expect("execute ok");
    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            assert_eq!(vars, vec!["s".to_owned()]);
            assert!(rows.is_empty(), "MemStore has no data, expected 0 rows");
        }
        other => panic!("expected Solutions, got {other:?}"),
    }
}

#[test]
fn accepts_triple_term_pattern_in_rdf12_mode() {
    let q = parse_select(
        "SELECT ?s WHERE { ?s <http://ex/claims> <<( <http://ex/Bob> <http://ex/age> 30 )>> }",
    );
    let alg = translate::translate_query_with(&q, &SparqlConfig::rdf12()).expect("translate ok");
    // The single triple has a triple-term object — the algebra Term enum
    // carries it as `Term::Triple(Box<TriplePattern>)`.
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    let patterns = match inner {
        Algebra::Bgp { patterns } => patterns,
        other => panic!("expected Bgp, got {other:?}"),
    };
    assert_eq!(patterns.len(), 1);
    let TriplePattern { object, .. } = &patterns[0];
    match object {
        Term::Triple(inner_tp) => {
            assert!(matches!(inner_tp.subject, Term::Iri(_)));
            assert!(matches!(inner_tp.predicate, Term::Iri(_)));
            assert!(matches!(inner_tp.object, Term::Literal(_)));
        }
        other => panic!("expected Term::Triple object, got {other:?}"),
    }
}
