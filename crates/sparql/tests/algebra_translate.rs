use horndb_sparql::algebra::{
    translate, Algebra, DatasetSpec, GraphSpec, Term, TranslatedQuery, TriplePattern,
};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::SparqlConfig;

fn alg_of(query: &str) -> Algebra {
    let q = parse_query(query).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe not supported here"),
        ParsedQuery::Explain { .. } => panic!("explain not supported here"),
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
fn minus_translates_to_algebra_minus() {
    // HDB-133: MINUS lowers to `Algebra::Minus`, an anti-join — not a
    // rejection, and not a rewrite into `Algebra::Filter`/NOT EXISTS.
    let alg = alg_of("SELECT * WHERE { ?s ?p ?o MINUS { ?s <http://ex/q> ?z } }");
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    match inner {
        Algebra::Minus { left, right } => {
            assert!(matches!(*left, Algebra::Bgp { .. }), "left: {left:?}");
            assert!(matches!(*right, Algebra::Bgp { .. }), "right: {right:?}");
        }
        other => panic!("expected Minus, got {other:?}"),
    }
}

#[test]
fn lowers_kleene_star_path_to_closure() {
    // `*` now lowers to an `Algebra::PathClosure` (SPEC-07 #50) rather
    // than being rejected. The reflexive flag marks `*` vs `+`.
    use horndb_sparql::algebra::Algebra;
    let q = parse_query("SELECT ?x WHERE { ?x <http://ex/p>* ?y }").expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate::translate_query(&inner).expect("translate");
    // Unwrap the outer Project/Distinct the path lowering wraps around it.
    fn find_closure(a: &Algebra) -> Option<bool> {
        match a {
            Algebra::PathClosure { reflexive, .. } => Some(*reflexive),
            Algebra::Project { inner, .. }
            | Algebra::Distinct { inner }
            | Algebra::Slice { inner, .. } => find_closure(inner),
            _ => None,
        }
    }
    assert_eq!(
        find_closure(&alg),
        Some(true),
        "expected a reflexive PathClosure, got: {alg:?}"
    );
}

#[test]
fn lowers_kleene_plus_path_to_closure() {
    use horndb_sparql::algebra::Algebra;
    let q = parse_query("SELECT ?x WHERE { ?x <http://ex/p>+ ?y }").expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate::translate_query(&inner).expect("translate");
    fn find_reflexive(a: &Algebra) -> Option<bool> {
        match a {
            Algebra::PathClosure { reflexive, .. } => Some(*reflexive),
            Algebra::Project { inner, .. }
            | Algebra::Distinct { inner }
            | Algebra::Slice { inner, .. } => find_reflexive(inner),
            _ => None,
        }
    }
    assert_eq!(
        find_reflexive(&alg),
        Some(false),
        "expected a non-reflexive (`+`) PathClosure, got: {alg:?}"
    );
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
        ParsedQuery::Explain { .. } => panic!("explain not supported here"),
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
    let alg = translate::translate_query_with(&q, &SparqlConfig::rdf12())
        .expect("translate ok")
        .algebra;
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

// SPEC-28 phase 3 (#266): `Algebra::Graph` + `DatasetSpec` capture. Ground
// and variable-form evaluation land in Task 3/4 (graph_query.rs); these
// pin translation structure and dataset-clause resolution only.

fn translated_of(query: &str) -> TranslatedQuery {
    let q = parse_query(query).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe not supported here"),
        ParsedQuery::Explain { .. } => panic!("explain not supported here"),
    };
    translate::translate_query_with(&inner, &SparqlConfig::default()).expect("translate")
}

#[test]
fn graph_iri_translates_to_graph_node() {
    let alg = translated_of("SELECT * WHERE { GRAPH <http://ex/g> { ?s ?p ?o } }").algebra;
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    match inner {
        Algebra::Graph { name, inner } => {
            assert_eq!(name, GraphSpec::Iri("http://ex/g".to_owned()));
            assert!(matches!(*inner, Algebra::Bgp { .. }), "expected Bgp inner");
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn graph_var_translates_and_scopes_var() {
    // GRAPH ?g under SELECT * must project ?g (`collect_visible_vars` scopes
    // the graph variable — now correct rather than vacuous, per the design).
    let alg = translated_of("SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }").algebra;
    match alg {
        Algebra::Project { vars, inner } => {
            let names: Vec<&str> = vars.iter().map(|v| v.name()).collect();
            assert!(
                names.contains(&"g"),
                "expected ?g among projected vars, got {names:?}"
            );
            match *inner {
                Algebra::Graph { name, .. } => {
                    assert!(matches!(name, GraphSpec::Var(v) if v.name() == "g"));
                }
                other => panic!("expected Graph, got {other:?}"),
            }
        }
        other => panic!("expected Project, got {other:?}"),
    }
}

#[test]
fn nested_graph_preserves_nesting_order() {
    // Translation preserves the nesting order (outer Graph wraps inner
    // Graph) rather than collapsing or reordering it. It does not itself
    // implement "innermost wins" — that's scan-scope lowering's job
    // (PLAN-28-03 Task 3, which overwrites the outer scope on the way
    // down); this only pins the tree shape lowering relies on.
    let alg = translated_of(
        "SELECT * WHERE { GRAPH <http://ex/g1> { GRAPH <http://ex/g2> { ?s ?p ?o } } }",
    )
    .algebra;
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    let (outer_name, outer_inner) = match inner {
        Algebra::Graph { name, inner } => (name, inner),
        other => panic!("expected outer Graph, got {other:?}"),
    };
    assert_eq!(outer_name, GraphSpec::Iri("http://ex/g1".to_owned()));
    match *outer_inner {
        Algebra::Graph { name, inner } => {
            assert_eq!(name, GraphSpec::Iri("http://ex/g2".to_owned()));
            assert!(matches!(*inner, Algebra::Bgp { .. }), "expected Bgp inner");
        }
        other => panic!("expected nested Graph, got {other:?}"),
    }
}

#[test]
fn from_clause_recorded() {
    // FROM list present -> default graph is exactly that list; named is
    // Some(vec![]), not None — any dataset clause makes both fields Some
    // (the representation-level invariant `DatasetSpec` documents).
    let tq = translated_of("SELECT * FROM <http://ex/g1> FROM <http://ex/g2> WHERE { ?s ?p ?o }");
    assert_eq!(
        tq.dataset,
        DatasetSpec {
            default: Some(vec!["http://ex/g1".to_owned(), "http://ex/g2".to_owned()]),
            named: Some(vec![]),
        }
    );
}

#[test]
fn from_named_only_yields_empty_default() {
    // D4: FROM NAMED without FROM narrows the default graph to empty,
    // distinct from "no dataset clause at all" (None).
    let tq = translated_of("SELECT * FROM NAMED <http://ex/g1> WHERE { ?s ?p ?o }");
    assert_eq!(
        tq.dataset,
        DatasetSpec {
            default: Some(vec![]),
            named: Some(vec!["http://ex/g1".to_owned()]),
        }
    );
}

#[test]
fn from_and_from_named_recorded_separately() {
    // Both clauses present: the only combination not covered by the two
    // tests above, and the one where a mis-partition of spargebra's flat
    // clause list (which interleaves FROM and FROM NAMED entries) would
    // show up — each graph must land in the right field.
    let tq =
        translated_of("SELECT * FROM <http://ex/g1> FROM NAMED <http://ex/g2> WHERE { ?s ?p ?o }");
    assert_eq!(
        tq.dataset,
        DatasetSpec {
            default: Some(vec!["http://ex/g1".to_owned()]),
            named: Some(vec!["http://ex/g2".to_owned()]),
        }
    );
}

#[test]
fn no_dataset_clause_yields_none_dataset() {
    // Regression guard for the third pinned rule: absent FROM/FROM NAMED
    // leaves both fields None so the executor's default-graph mode and
    // visibility rules decide (SPEC-28 S3), rather than defaulting to an
    // empty selection.
    let tq = translated_of("SELECT * WHERE { ?s ?p ?o }");
    assert_eq!(
        tq.dataset,
        DatasetSpec {
            default: None,
            named: None,
        }
    );
}

#[test]
fn translate_query_rejects_dataset_clause() {
    // translate_query's return type has no room for a DatasetSpec, so it
    // must refuse a query naming FROM/FROM NAMED rather than silently
    // drop it — a caller who planned and ran the returned algebra would
    // get the configured default dataset instead of the one the query
    // named. translate_query_with is the safe path: it returns the
    // DatasetSpec alongside the algebra and cannot lose it.
    let q = parse_query("SELECT ?s FROM <http://ex/g> WHERE { ?s ?p ?o }").expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        other => panic!("expected Select, got {other:?}"),
    };
    let err = translate::translate_query(&inner).unwrap_err();
    assert!(
        err.to_string().contains("translate_query_with"),
        "expected the error to point at translate_query_with, got: {err}"
    );
}
