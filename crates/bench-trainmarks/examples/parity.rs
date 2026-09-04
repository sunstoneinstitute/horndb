//! Throwaway parity check: prints sorted rows for q1-q5 and the triple count after q6.
use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_storage::loader::ntriples::for_each_ntriples_batch;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).unwrap();
    let mut backend = HornBackend::new();
    let mut all: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> = Vec::new();
    for_each_ntriples_batch(&bytes, 4, |triples: Vec<oxrdf::Triple>| {
        all.extend(triples.into_iter().map(|t| {
            (
                oxrdf::Term::from(t.subject),
                oxrdf::Term::from(t.predicate),
                t.object,
            )
        }));
        Ok(())
    })
    .unwrap();
    backend.insert_oxrdf_batch(all).unwrap();
    for q in [
        "q1_count",
        "q2_customer_orders",
        "q3_join_3_entities",
        "q4_optional_aggregation",
        "q5_construct",
    ] {
        let sql = std::fs::read_to_string(format!("{}/{q}.rq", args[2])).unwrap();
        let mut lines: Vec<String> = match execute_query(&sql, &backend).unwrap() {
            QueryAnswer::Solutions { rows, .. } => {
                rows.iter().map(|r| format!("{q} {r:?}")).collect()
            }
            QueryAnswer::Triples(ts) => ts.iter().map(|t| format!("{q} {t:?}")).collect(),
            QueryAnswer::Boolean(b) => vec![format!("{q} {b}")],
            QueryAnswer::Explanation { .. } => vec![format!("{q} explain")],
        };
        lines.sort();
        for l in lines {
            println!("{l}");
        }
    }
    let sql = std::fs::read_to_string(format!("{}/q6_delete_insert.rq", args[2])).unwrap();
    execute_update(&sql, &mut backend).unwrap();
    println!("q6 triples after update: {}", backend.iter_oxrdf().len());
}
