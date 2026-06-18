use horndb_sparql::parser::{parse_query, parse_update, ParsedQuery, ParsedUpdate};

#[test]
fn parses_minimal_select() {
    let q = parse_query("SELECT ?s WHERE { ?s ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Select { .. }));
}

#[test]
fn parses_ask() {
    let q = parse_query("ASK { ?s ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Ask { .. }));
}

#[test]
fn parses_construct() {
    let q = parse_query("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Construct { .. }));
}

#[test]
fn rejects_garbage_query() {
    let err = parse_query("THIS IS NOT SPARQL").unwrap_err();
    assert!(format!("{err}").contains("parse error"));
}

#[test]
fn parses_insert_data() {
    let u = parse_update("INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }")
        .expect("must parse");
    assert!(matches!(u, ParsedUpdate::InsertData { .. }));
}

#[test]
fn parses_delete_data() {
    let u = parse_update("DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }")
        .expect("must parse");
    assert!(matches!(u, ParsedUpdate::DeleteData { .. }));
}

#[test]
fn classifies_delete_insert_where() {
    let u = parse_update(
        "DELETE { ?s <http://ex/p> ?o } INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    assert!(matches!(u, ParsedUpdate::DeleteInsert { .. }));
}

#[test]
fn classifies_delete_where_shorthand() {
    let u = parse_update("DELETE WHERE { ?s <http://ex/p> ?o }").unwrap();
    assert!(matches!(u, ParsedUpdate::DeleteInsert { .. }));
}

#[test]
fn parses_explain_pragma_text() {
    let q = parse_query("EXPLAIN SELECT ?s WHERE { ?s ?p ?o }").expect("must parse");
    match q {
        ParsedQuery::Explain { inner, json } => {
            assert!(!json, "plain EXPLAIN must not request JSON");
            assert!(matches!(*inner, ParsedQuery::Select { .. }));
        }
        other => panic!("expected Explain, got {other:?}"),
    }
}

#[test]
fn parses_explain_json_pragma() {
    let q = parse_query("EXPLAIN JSON SELECT ?s WHERE { ?s ?p ?o }").expect("must parse");
    match q {
        ParsedQuery::Explain { inner, json } => {
            assert!(json, "EXPLAIN JSON must request JSON");
            assert!(matches!(*inner, ParsedQuery::Select { .. }));
        }
        other => panic!("expected Explain, got {other:?}"),
    }
}

#[test]
fn explain_pragma_is_case_insensitive_and_tolerates_leading_ws() {
    let q = parse_query("  explain  ask { ?s ?p ?o }").expect("must parse");
    match q {
        ParsedQuery::Explain { inner, json } => {
            assert!(!json);
            assert!(matches!(*inner, ParsedQuery::Ask { .. }));
        }
        other => panic!("expected Explain, got {other:?}"),
    }
}

#[test]
fn explain_over_construct_is_recognised() {
    let q = parse_query("EXPLAIN CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }").expect("must parse");
    match q {
        ParsedQuery::Explain { inner, .. } => {
            assert!(matches!(*inner, ParsedQuery::Construct { .. }));
        }
        other => panic!("expected Explain, got {other:?}"),
    }
}

#[test]
fn explain_prefix_in_identifier_is_not_a_pragma() {
    // A SELECT whose first projected variable starts with "explain" must
    // not be mistaken for the pragma — the keyword needs a whitespace
    // boundary, and `EXPLAINME` has none.
    let q = parse_query("SELECT ?explainme WHERE { ?explainme ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Select { .. }));
}

#[test]
fn bare_explain_with_no_query_is_a_parse_error() {
    // EXPLAIN must wrap a real query; `EXPLAIN ` alone is a parse error
    // from the inner parse.
    let err = parse_query("EXPLAIN   ").unwrap_err();
    assert!(format!("{err}").contains("parse error"), "{err}");
}
