//! A `HornBackend` bulk load must intern each term exactly once (HDB-87).
//!
//! `insert_oxrdf_batch` has to intern anyway, to build the `QuadKey`s it
//! deduplicates on. It therefore passes the resulting ids to storage
//! (`Store::insert_quad_ids`) instead of handing the terms back for a second,
//! identical dictionary pass. The `intern` load phase is emitted only by the
//! term-based `Store::apply_quads`, so it stays at zero on this path — that is
//! what the counter assertion below pins.
//!
//! Both halves run in one test function on purpose: the `storage_load_phase_*`
//! counters are process-global, so a second test running concurrently in this
//! binary could move them.

use horndb_metrics::labels::{LoadPhase, LoadPhaseLabel};
use horndb_sparql::exec::horn::HornBackend;
use horndb_storage::{Store, DEFAULT_GRAPH};
use oxrdf::{Literal, NamedNode, Term};

fn intern_rows() -> u64 {
    horndb_metrics::metrics()
        .storage
        .load_phase_rows
        .get_or_create(&LoadPhaseLabel {
            phase: LoadPhase::Intern,
        })
        .get()
}

/// Repeated subjects and predicates plus one exact duplicate triple, so the
/// load exercises dictionary hits, misses and intra-batch dedup.
fn corpus() -> Vec<(Term, Term, Term)> {
    let iri = |s: &str| Term::NamedNode(NamedNode::new(s).unwrap());
    let mut out = Vec::new();
    for i in 0..200 {
        let s = iri(&format!("http://example.org/s{}", i % 40));
        out.push((
            s.clone(),
            iri(&format!("http://example.org/p{}", i % 4)),
            iri(&format!("http://example.org/o{i}")),
        ));
        out.push((
            s,
            iri("http://example.org/label"),
            Term::Literal(Literal::new_simple_literal(format!("l{}", i % 9))),
        ));
    }
    out.push(out[0].clone());
    out
}

fn sorted_text(triples: &[(Term, Term, Term)]) -> Vec<String> {
    let mut v: Vec<String> = triples
        .iter()
        .map(|(s, p, o)| format!("{s} {p} {o}"))
        .collect();
    v.sort();
    v
}

#[test]
fn bulk_load_does_not_re_intern_and_matches_the_term_path() {
    let doc = corpus();
    let distinct = {
        let mut v = sorted_text(&doc);
        v.dedup();
        v.len() as u64
    };

    let before = intern_rows();
    let mut backend = HornBackend::new();
    let inserted = backend.insert_oxrdf_batch(doc.clone()).unwrap();
    let after_backend = intern_rows();
    assert_eq!(inserted, distinct);
    assert_eq!(
        after_backend, before,
        "HornBackend bulk load re-interned via Store::apply_quads; it must pass \
         the ids it already resolved"
    );

    // Control: the term-based store path *does* move the counter, so the
    // assertion above is a real observation and not a dead metric.
    let term_path = Store::in_memory();
    let quads: Vec<_> = doc
        .iter()
        .map(|(s, p, o)| (DEFAULT_GRAPH, s.clone(), p.clone(), o.clone()))
        .collect();
    term_path.insert_quads(&quads).unwrap();
    assert!(intern_rows() > after_backend);

    // Same answers out of both paths.
    let from_backend = sorted_text(&backend.iter_oxrdf());
    let d = term_path.dictionary();
    let from_store: Vec<(Term, Term, Term)> = term_path
        .scan_all_term_ids()
        .into_iter()
        .map(|(s, p, o)| {
            (
                d.lookup(s).unwrap(),
                d.lookup(p).unwrap(),
                d.lookup(o).unwrap(),
            )
        })
        .collect();
    assert_eq!(from_backend, sorted_text(&from_store));
    assert_eq!(backend.len(), term_path.triple_count());
}
