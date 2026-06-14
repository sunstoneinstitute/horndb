//! SPEC-02 NF1: cold-tier (snapshot) footprint ≤ 6 bytes/triple amortised on a
//! representative corpus.

use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

fn iri(s: String) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn snapshot_footprint_under_six_bytes_per_triple() {
    let store = Store::in_memory();
    let base = "http://www.lehigh.edu/univ-bench";
    let type_p = iri(format!("{base}#type"));
    let advisor_p = iri(format!("{base}#advisor"));
    let member_p = iri(format!("{base}#memberOf"));
    let takes_p = iri(format!("{base}#takesCourse"));

    let mut triples = Vec::new();
    // 10 universities, 20 departments each, 50 students each => 10000 students,
    // each with several edges -> 40k triples with heavy IRI prefix sharing.
    // Courses (mod 12) and professors (mod 6) are shared within a department, so
    // a larger student cohort amortises the dictionary across more reused terms
    // (realistic for larger LUBM scale factors).
    for u in 0..10 {
        for d in 0..20 {
            let dept = iri(format!("{base}/University{u}/Department{d}"));
            for s in 0..50 {
                let student = iri(format!(
                    "{base}/University{u}/Department{d}/GraduateStudent{s}"
                ));
                let course = iri(format!(
                    "{base}/University{u}/Department{d}/Course{}",
                    s % 12
                ));
                let prof = iri(format!(
                    "{base}/University{u}/Department{d}/Professor{}",
                    s % 6
                ));
                let grad = iri(format!("{base}#GraduateStudent"));
                triples.push((student.clone(), type_p.clone(), grad));
                triples.push((student.clone(), member_p.clone(), dept.clone()));
                triples.push((student.clone(), advisor_p.clone(), prof));
                triples.push((student.clone(), takes_p.clone(), course));
            }
        }
    }
    store.insert_triples(&triples).unwrap();

    let mut bytes = Vec::new();
    let stats = store.export_snapshot(&mut bytes).unwrap();
    let bpt = stats.bytes_per_triple();
    eprintln!(
        "snapshot: {} triples, {} distinct terms, dict {} B, triples {} B, total {} B => {:.3} B/triple",
        stats.triples,
        stats.distinct_terms,
        stats.dictionary_bytes,
        stats.triples_bytes,
        stats.total_bytes,
        bpt
    );
    assert!(
        bpt <= 6.0,
        "snapshot footprint {bpt:.3} B/triple exceeds NF1 budget of 6.0"
    );
    assert_eq!(bytes.len() as u64, stats.total_bytes);
}
