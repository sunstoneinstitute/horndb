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
