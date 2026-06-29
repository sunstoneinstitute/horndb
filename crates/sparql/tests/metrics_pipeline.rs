#![cfg(feature = "server")]
use horndb_sparql::api::execute_query;
use horndb_sparql::exec::mem::MemStore;

#[test]
fn query_kind_and_stage_metrics_recorded() {
    let store = MemStore::default();
    let q = "SELECT * WHERE { ?s ?p ?o } LIMIT 1";
    // Real signature is `execute_query(query, exec)` (query first).
    let _ = execute_query(q, &store).expect("query ok");

    let text = horndb_metrics::encode_metrics();
    assert!(text.contains("horndb_sparql_query_total"), "got:\n{text}");
    assert!(text.contains("kind=\"select\""), "got:\n{text}");
    assert!(
        text.contains("horndb_sparql_stage_duration_seconds"),
        "got:\n{text}"
    );
}
