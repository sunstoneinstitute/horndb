//! End-to-end coverage for the non-standard `EXPLAIN` pragma
//! (SPEC-07 F9 / acceptance #5). `EXPLAIN` returns the chosen physical
//! plan with the execution mode and per-node cardinality estimates,
//! without executing the query.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

/// A small class hierarchy for the acceptance-#5 `subClassOf+` shape:
///   A ⊑ B ⊑ C ⊑ D
fn class_hierarchy() -> MemStore {
    let mut s = MemStore::default();
    let sco = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    for (sub, sup) in [("A", "B"), ("B", "C"), ("C", "D")] {
        s.insert_triple(iri(sub), iri(sco), iri(sup));
    }
    // Some noise that the explained query should not scan.
    s.insert_triple(iri("X"), iri("http://ex/type"), iri("T"));
    s
}

#[test]
fn explain_select_returns_plan_not_results() {
    let store = class_hierarchy();
    let ans = execute_query("EXPLAIN SELECT ?o WHERE { ?s ?p ?o }", &store).unwrap();
    match ans {
        QueryAnswer::Explanation { text, json } => {
            assert!(!json);
            assert!(text.contains("EXPLAIN"), "{text}");
            assert!(text.contains("mode: materialized"), "{text}");
            assert!(text.contains("Project(?o)"), "{text}");
            assert!(text.contains("BgpScan"), "{text}");
            // cardinality estimates are present and `~`-labelled.
            assert!(text.contains("rows"), "{text}");
            assert!(text.contains('~'), "{text}");
        }
        other => panic!("EXPLAIN must return Explanation, got {other:?}"),
    }
}

#[test]
fn explain_does_not_execute_the_query() {
    // If EXPLAIN executed, an always-true ASK would surface as a Boolean.
    // It must instead return the plan.
    let store = class_hierarchy();
    let ans = execute_query("EXPLAIN ASK { ?s ?p ?o }", &store).unwrap();
    assert!(
        matches!(ans, QueryAnswer::Explanation { .. }),
        "EXPLAIN ASK must not run the ASK"
    );
}

#[test]
fn explain_recursive_path_shows_mode_and_cardinality() {
    // SPEC-07 acceptance #5: EXPLAIN on `subClassOf+` clearly shows the
    // chosen mode and cardinality estimates.
    let store = class_hierarchy();
    let q = "EXPLAIN SELECT ?x ?y WHERE { \
             ?x <http://www.w3.org/2000/01/rdf-schema#subClassOf>+ ?y }";
    let ans = execute_query(q, &store).unwrap();
    match ans {
        QueryAnswer::Explanation { text, .. } => {
            assert!(text.contains("PathClosure"), "{text}");
            assert!(text.contains("transitive"), "{text}");
            assert!(text.contains("mode: materialized"), "{text}");
            // The closure scans the three subClassOf edges.
            assert!(
                text.contains("~3 rows"),
                "plan should estimate 3 sco edges: {text}"
            );
        }
        other => panic!("expected Explanation, got {other:?}"),
    }
}

#[test]
fn explain_json_is_valid_json() {
    let store = class_hierarchy();
    let ans = execute_query("EXPLAIN JSON SELECT ?o WHERE { ?s ?p ?o }", &store).unwrap();
    match ans {
        QueryAnswer::Explanation { text, json } => {
            assert!(json, "EXPLAIN JSON must set the json flag");
            let v: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert_eq!(v["mode"], "materialized");
            assert_eq!(v["backwardChainingAvailable"], false);
            assert!(v["plan"]["op"].is_string());
            assert!(v["plan"]["estRows"].is_number());
        }
        other => panic!("expected Explanation, got {other:?}"),
    }
}
