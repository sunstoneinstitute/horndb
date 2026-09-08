//! HDB-229: the direct source must not index predicates the query cannot read.
//!
//! `?s <p> <o>` binds predicate and object, so the cursor reads an
//! *object-major* ordering and seeks straight to `<p>`'s leaf. Opening that
//! ordering used to materialize the object-major `(o, s)` layout of **every**
//! predicate partition in the graph — 32 B/row over the whole graph, retained
//! for the life of the process, for leaves the cursor never opens.
//!
//! `HornBackend::bgp_predicates` now narrows the source to the predicates the
//! BGP names. Correctness of the narrowing is `direct_source_parity.rs`'s job
//! (it compares the direct source against the `VecTripleSource` oracle over a
//! whole query battery); this file pins the footprint and the one rule that
//! makes the narrowing sound — a free predicate must keep every leaf.

use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;

/// Rows on the predicate the queries name.
const HOT: u64 = 100;
/// Rows on each predicate they do not.
const COLD: u64 = 20_000;
const PREDICATES: u64 = 2;
const GRAPH_ROWS: u64 = HOT + PREDICATES * COLD;
/// What materializing one row's object-major layout costs the tier: the
/// re-sorted `(o, s)` pair plus its `(begin, end)` stamps.
const OBJECT_MAJOR_BYTES_PER_ROW: u64 = 32;

fn iri(v: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v))
}

/// One small predicate (`p0`, every row sharing object `o`) and two large ones.
/// The size gap is what makes a whole-graph index build a number rather than
/// noise.
fn fixture() -> HornBackend {
    let mut b = HornBackend::new();
    b.set_direct_source(true);
    let mut triples = Vec::new();
    for i in 0..HOT {
        triples.push((
            iri(&format!("http://ex/s{i}")),
            iri("http://ex/p0"),
            iri("http://ex/o"),
        ));
    }
    for p in 1..=PREDICATES {
        for i in 0..COLD {
            triples.push((
                iri(&format!("http://ex/s{i}")),
                iri(&format!("http://ex/p{p}")),
                iri(&format!("http://ex/o{i}")),
            ));
        }
    }
    b.insert_oxrdf_batch(triples).unwrap();
    b
}

fn rows(b: &HornBackend, q: &str) -> usize {
    match execute_query(q, b).unwrap() {
        QueryAnswer::Solutions { rows, .. } => rows.len(),
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Growth in the tier's byte estimate across running `q`.
fn indexing_cost(b: &HornBackend, q: &str, expect_rows: usize) -> u64 {
    let before = b.storage_stats().bytes_estimated;
    assert_eq!(rows(b, q), expect_rows, "{q}");
    b.storage_stats().bytes_estimated - before
}

#[test]
fn a_bound_predicate_indexes_only_its_own_partition() {
    let b = fixture();
    let cost = indexing_cost(
        &b,
        "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> }",
        HOT as usize,
    );

    let whole_graph = GRAPH_ROWS * OBJECT_MAJOR_BYTES_PER_ROW;
    assert!(
        cost <= HOT * OBJECT_MAJOR_BYTES_PER_ROW,
        "reading one predicate ({HOT} rows) indexed {cost} B; its own partition \
         is {} B and the whole graph is {whole_graph} B (HDB-229)",
        HOT * OBJECT_MAJOR_BYTES_PER_ROW
    );
}

/// A join naming two predicates may index those two, and no others.
#[test]
fn a_join_indexes_only_the_predicates_it_names() {
    let b = fixture();
    let cost = indexing_cost(
        &b,
        "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> . ?s <http://ex/p1> ?z }",
        HOT as usize,
    );

    let named = (HOT + COLD) * OBJECT_MAJOR_BYTES_PER_ROW;
    let whole_graph = GRAPH_ROWS * OBJECT_MAJOR_BYTES_PER_ROW;
    assert!(
        cost <= named,
        "a join over two predicates indexed {cost} B; those two partitions are \
         {named} B and the whole graph is {whole_graph} B (HDB-229)"
    );
}

/// The soundness rule: a free predicate ranges over the whole graph, so every
/// leaf must stay. This is the case the narrowing must refuse to apply — a
/// missing leaf would silently drop rows, which is worse than the footprint.
#[test]
fn a_free_predicate_keeps_every_leaf() {
    let b = fixture();
    // `?s ?p <o>` matches only p0's rows, but it must reach them by scanning
    // every predicate — proving the source was not narrowed.
    assert_eq!(
        rows(&b, "SELECT ?s ?p WHERE { ?s ?p <http://ex/o> }"),
        HOT as usize
    );
    // And a free predicate over an object only the large partitions carry
    // must still find it.
    assert_eq!(
        rows(&b, "SELECT ?s ?p WHERE { ?s ?p <http://ex/o7> }"),
        PREDICATES as usize
    );
}

/// Consecutive queries over different predicates each get their own leaf set:
/// the restriction is part of the source cache key, so the second must not be
/// answered from the first's narrower source.
#[test]
fn a_second_query_over_another_predicate_sees_its_own_leaves() {
    let b = fixture();
    assert_eq!(
        rows(&b, "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> }"),
        HOT as usize
    );
    assert_eq!(
        rows(&b, "SELECT ?s WHERE { ?s <http://ex/p1> <http://ex/o7> }"),
        1
    );
    assert_eq!(
        rows(&b, "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> }"),
        HOT as usize
    );
}
