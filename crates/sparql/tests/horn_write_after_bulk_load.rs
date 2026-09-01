//! `INSERT DATA` idempotency and `DELETE DATA` no-op detection must still hold
//! for writes that arrive *after* a bulk load (HDB-89).
//!
//! Until HDB-89 the backend answered both questions from `live_keys`, a
//! `HashSet` mirror of every live quad that the bulk load populated one entry
//! at a time. Removing it moved both answers to storage
//! (`Tier::apply_quad_batch` inserts only what is not already visible and
//! reports the true counts; `StoreSnapshot::contains_quad` is the point read).
//! The interaction that regression puts at risk is exactly this one: load in
//! bulk, then write. Both operations are covered in isolation elsewhere
//! (`update_insert_delete.rs`, `update_named_graph.rs`); the sequence is what
//! is new here.

use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{AlgebraQuad, Store};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use oxrdf::{NamedNode, Term as OxTerm};

use horndb_sparql::algebra::Term as ATerm;

fn iri(s: &str) -> OxTerm {
    OxTerm::NamedNode(NamedNode::new(s).unwrap())
}

fn quad(s: &str, p: &str, o: &str) -> AlgebraQuad {
    (
        None,
        ATerm::Iri(s.into()),
        ATerm::Iri(p.into()),
        ATerm::Iri(o.into()),
    )
}

/// 20,000 default-graph triples over 4 predicates, enough that the load goes
/// through the batch path and every predicate partition holds real rows.
fn corpus() -> Vec<(OxTerm, OxTerm, OxTerm)> {
    (0..20_000)
        .map(|i| {
            (
                iri(&format!("http://ex/s{}", i % 2_000)),
                iri(&format!("http://ex/p{}", i % 4)),
                iri(&format!("http://ex/o{i}")),
            )
        })
        .collect()
}

fn loaded() -> HornBackend {
    let mut b = HornBackend::new();
    let rows = corpus();
    let n = b.insert_oxrdf_batch(rows.clone()).unwrap();
    assert_eq!(n, rows.len() as u64, "bulk load must report every new row");
    assert_eq!(b.len(), rows.len() as u64);
    b
}

#[test]
fn insert_data_is_idempotent_after_a_bulk_load() {
    let mut b = loaded();
    let before = b.len();

    // A triple the bulk load already made live.
    let counts = b
        .apply_quads(
            vec![],
            vec![quad("http://ex/s0", "http://ex/p0", "http://ex/o0")],
        )
        .unwrap();
    assert_eq!(counts.inserted, 0, "already live: INSERT DATA is a no-op");
    assert_eq!(b.len(), before);

    // The same thing through the SPARQL surface.
    apply_update(
        &parse_update("INSERT DATA { <http://ex/s0> <http://ex/p0> <http://ex/o0> }").unwrap(),
        &mut b,
    )
    .unwrap();
    assert_eq!(b.len(), before);

    // And through the single-triple backend entry point, which reports
    // "was it new?" as its return value.
    assert!(
        !b.insert_oxrdf(
            &iri("http://ex/s0"),
            &iri("http://ex/p0"),
            &iri("http://ex/o0")
        )
        .unwrap(),
        "already live: insert_oxrdf must report not-new"
    );
    assert_eq!(b.len(), before);

    // Re-running the whole load changes nothing.
    assert_eq!(b.insert_oxrdf_batch(corpus()).unwrap(), 0);
    assert_eq!(b.len(), before);

    // A genuinely new triple still lands.
    assert!(b
        .insert_oxrdf(
            &iri("http://ex/s0"),
            &iri("http://ex/p0"),
            &iri("http://ex/fresh")
        )
        .unwrap());
    assert_eq!(b.len(), before + 1);
}

#[test]
fn delete_data_detects_a_no_op_after_a_bulk_load() {
    let mut b = loaded();
    let before = b.len();

    // Absent triple: nothing retracted, nothing else disturbed. Both the
    // subject and the object are known terms, so this is not answered by a
    // dictionary miss.
    let counts = b
        .apply_quads(
            vec![quad("http://ex/s0", "http://ex/p0", "http://ex/o1")],
            vec![],
        )
        .unwrap();
    assert_eq!(counts.retracted, 0, "absent quad: DELETE DATA is a no-op");
    assert_eq!(b.len(), before);

    // Present triple: retracted exactly once.
    let counts = b
        .apply_quads(
            vec![quad("http://ex/s0", "http://ex/p0", "http://ex/o0")],
            vec![],
        )
        .unwrap();
    assert_eq!(counts.retracted, 1);
    assert_eq!(b.len(), before - 1);

    // Retracting it again is a no-op, not a second decrement.
    apply_update(
        &parse_update("DELETE DATA { <http://ex/s0> <http://ex/p0> <http://ex/o0> }").unwrap(),
        &mut b,
    )
    .unwrap();
    assert_eq!(b.len(), before - 1);
}

/// A retracted row stays in the partition as history, so the point read that
/// decides idempotency has to look past it. Insert / delete / insert / insert.
#[test]
fn reinsert_after_delete_is_live_again_then_idempotent() {
    let mut b = loaded();
    let before = b.len();
    let (s, p, o) = ("http://ex/s0", "http://ex/p0", "http://ex/o0");

    assert_eq!(
        b.apply_quads(vec![quad(s, p, o)], vec![])
            .unwrap()
            .retracted,
        1
    );
    assert_eq!(b.len(), before - 1);

    assert!(
        b.insert_oxrdf(&iri(s), &iri(p), &iri(o)).unwrap(),
        "retracted then re-inserted: must count as new"
    );
    assert_eq!(b.len(), before);

    assert!(
        !b.insert_oxrdf(&iri(s), &iri(p), &iri(o)).unwrap(),
        "live again: the second insert must be a no-op"
    );
    assert_eq!(b.len(), before);
}
