//! Sanity check on Stage-1 query latency for a small synthetic dataset.
//!
//! We do NOT promise the SPEC-07 NF1 (≤2× GraphDB on SPB SF3) at this
//! stage — that's a Stage-2 commitment. This test only catches gross
//! regressions: 10k triples + a single-pattern SELECT in <1 s on any
//! reasonable laptop.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use std::time::Instant;

#[test]
fn ten_thousand_triple_scan_in_under_one_second() {
    let mut s = MemStore::default();
    for i in 0..10_000_u32 {
        s.insert_triple(
            Term::Iri(format!("http://ex/s{i}")),
            Term::Iri("http://ex/p".into()),
            Term::Iri(format!("http://ex/o{i}")),
        );
    }
    let q = "SELECT ?o WHERE { <http://ex/s5000> <http://ex/p> ?o }";
    let t = Instant::now();
    let ans = execute_query(q, &s).unwrap();
    let elapsed = t.elapsed();
    match ans {
        QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 1),
        other => panic!("unexpected: {other:?}"),
    }
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "latency {elapsed:?} exceeds 1s budget — investigate"
    );
}
