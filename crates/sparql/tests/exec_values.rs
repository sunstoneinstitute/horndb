//! Tests for the VALUES row source (`PhysicalPlan::Values`).
//!
//! Covers the UNDEF → unbound case that the native-slot port must preserve.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

/// VALUES with a mix of bound IRIs and UNDEF must return the right number of
/// rows, with UNDEF cells absent from the solution mapping (no binding for
/// that variable in that row).
#[test]
fn values_undef_produces_unbound_slot() {
    let store = MemStore::default(); // empty — VALUES needs no triples
    let q = r#"
        SELECT ?x ?y WHERE {
            VALUES (?x ?y) {
                (<http://ex/a> UNDEF)
                (<http://ex/b> <http://ex/c>)
            }
        }
    "#;
    let rows = match execute_query(q, &store).expect("query ok") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected Solutions, got {other:?}"),
    };

    assert_eq!(rows.len(), 2, "two rows from VALUES");

    // Row 0: x = <http://ex/a>, y = UNDEF (not bound)
    assert_eq!(
        rows[0].get("x"),
        Some(&iri("http://ex/a")),
        "row 0 ?x = <http://ex/a>"
    );
    assert_eq!(rows[0].get("y"), None, "row 0 ?y is UNDEF — must be absent");

    // Row 1: x = <http://ex/b>, y = <http://ex/c>
    assert_eq!(
        rows[1].get("x"),
        Some(&iri("http://ex/b")),
        "row 1 ?x = <http://ex/b>"
    );
    assert_eq!(
        rows[1].get("y"),
        Some(&iri("http://ex/c")),
        "row 1 ?y = <http://ex/c>"
    );
}

/// A single-variable VALUES clause must bind correctly for all rows.
#[test]
fn values_single_var_all_bound() {
    let store = MemStore::default();
    let q = r#"
        SELECT ?v WHERE {
            VALUES (?v) { (<http://ex/one>) (<http://ex/two>) (<http://ex/three>) }
        }
    "#;
    let rows = match execute_query(q, &store).expect("query ok") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected Solutions, got {other:?}"),
    };

    assert_eq!(rows.len(), 3, "three rows");
    let got: Vec<_> = rows
        .iter()
        .filter_map(|r| match r.get("v") {
            Some(Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        vec!["http://ex/one", "http://ex/two", "http://ex/three"],
        "bound values in order"
    );
}
