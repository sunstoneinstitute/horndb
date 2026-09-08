//! HDB-229: a `COUNT` over one predicate must cost one predicate.
//!
//! `?s <p> <o>` binds predicate and object, so the trie serves it from an
//! *object-major* ordering — and producing one materializes the object-major
//! layout of **every** predicate in the graph (direct source), or derives a
//! whole-scope second ordering (`VecTripleSource`). At LDBC SPB scale that was
//! 16.6 GiB and 67 s to answer with a single integer.
//!
//! `HornBackend::count_one_partition` answers the shape off the named
//! partition instead. These tests pin both halves: the footprint it must not
//! grow, and the counts it must still get right.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;

/// Rows on the predicate the query names.
const HOT: u64 = 100;
/// Rows on each predicate it never names.
const COLD: u64 = 20_000;
const PREDICATES: u64 = 2;
const GRAPH_ROWS: u64 = HOT + PREDICATES * COLD;

fn iri(v: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v))
}

/// One small predicate (`p0`, every row sharing object `o`) and two large ones
/// the queries never mention. The size gap is what makes a whole-graph index
/// build visible as a number rather than as noise.
fn fixture(direct: bool) -> HornBackend {
    let mut b = HornBackend::new();
    b.set_direct_source(direct);
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

fn count(b: &HornBackend, q: &str) -> u64 {
    match execute_query(q, b).unwrap() {
        QueryAnswer::Solutions { rows, .. } => {
            assert_eq!(rows.len(), 1, "a count is one row: {rows:?}");
            // `?n` is an `xsd:integer` in N-Triples form: `"100"^^<...>`.
            let Some(Term::Literal(lit)) = rows[0].get("n") else {
                panic!("?n must be a literal, got {:?}", rows[0].get("n"))
            };
            lit.trim_start_matches('"')
                .split('"')
                .next()
                .expect("literal lexical form")
                .parse()
                .expect("integer count")
        }
        other => panic!("expected solutions, got {other:?}"),
    }
}

fn rows(b: &HornBackend, q: &str) -> usize {
    match execute_query(q, b).unwrap() {
        QueryAnswer::Solutions { rows, .. } => rows.len(),
        other => panic!("expected solutions, got {other:?}"),
    }
}

const COUNT_PO: &str = "SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/p0> <http://ex/o> }";

/// The direct source materializes the object-major layout inside each
/// partition, so the cost lands in the tier's own byte estimate: +32 B per
/// row of every partition it builds.
#[test]
fn direct_count_does_not_index_predicates_it_never_reads() {
    let b = fixture(true);
    let before = b.storage_stats().bytes_estimated;
    assert_eq!(count(&b, COUNT_PO), HOT);
    let growth = b.storage_stats().bytes_estimated - before;

    let whole_graph = GRAPH_ROWS * 32;
    assert!(
        growth < whole_graph / 10,
        "counting one predicate ({HOT} rows) grew the tier by {growth} B; \
         indexing the whole graph would cost {whole_graph} B (HDB-229)"
    );
}

/// The `VecTripleSource` path keeps its orderings in the memoised snapshot,
/// so the same over-build shows up as a second whole-scope ordering there
/// (24 B/row) rather than in the tier.
#[test]
fn memoised_count_does_not_derive_a_second_whole_scope_ordering() {
    let b = fixture(false);
    let before = b.memory_split().snapshots;
    assert_eq!(count(&b, COUNT_PO), HOT);
    let growth = b.memory_split().snapshots - before;

    // One anchor ordering is the price of the memo itself; a second is the
    // bug. Allow the anchor, refuse the derivation.
    let one_ordering = GRAPH_ROWS * 24;
    assert!(
        growth <= one_ordering + one_ordering / 10,
        "counting one predicate grew the snapshot memo by {growth} B; \
         one whole-scope ordering is {one_ordering} B (HDB-229)"
    );
}

/// Every shape the fast path can claim, and the neighbours it must not, must
/// agree with the row count of the same BGP scanned out in full.
#[test]
fn counts_agree_with_the_scan_in_both_source_modes() {
    for direct in [false, true] {
        let mut b = fixture(direct);
        b.insert_oxrdf_batch(vec![
            // A second object on p0, so `?s <p0> ?o` and `?s <p0> <o>` differ.
            (
                iri("http://ex/x"),
                iri("http://ex/p0"),
                iri("http://ex/other"),
            ),
            // A duplicate of a row already live: storage must not double-count.
            (iri("http://ex/s0"), iri("http://ex/p0"), iri("http://ex/o")),
        ])
        .unwrap();
        execute_update(
            "DELETE DATA { <http://ex/s1> <http://ex/p0> <http://ex/o> }",
            &mut b,
        )
        .unwrap();

        let cases = [
            // Claimed by the fast path: predicate bound, subject free.
            (COUNT_PO, "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> }"),
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/p0> ?o }",
                "SELECT ?s ?o WHERE { ?s <http://ex/p0> ?o }",
            ),
            // Predicate bound but absent from the store: zero, not an error.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/nope> <http://ex/o> }",
                "SELECT ?s WHERE { ?s <http://ex/nope> <http://ex/o> }",
            ),
            // Not claimed: subject bound (the trie's binary search keeps it).
            (
                "SELECT (COUNT(*) AS ?n) WHERE { <http://ex/s0> <http://ex/p0> ?o }",
                "SELECT ?o WHERE { <http://ex/s0> <http://ex/p0> ?o }",
            ),
            // Not claimed: variable predicate.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p <http://ex/o> }",
                "SELECT ?s ?p WHERE { ?s ?p <http://ex/o> }",
            ),
            // Not claimed: more than one pattern.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/p0> <http://ex/o> . ?s <http://ex/p1> ?z }",
                "SELECT ?s ?z WHERE { ?s <http://ex/p0> <http://ex/o> . ?s <http://ex/p1> ?z }",
            ),
        ];
        for (count_q, scan_q) in cases {
            assert_eq!(
                count(&b, count_q) as usize,
                rows(&b, scan_q),
                "direct={direct}: {count_q}"
            );
        }
    }
}

/// A named-graph scope must count that graph only. `GRAPH <g>` resolves to a
/// single graph the fast path can claim; the default graph must not leak in.
#[test]
fn graph_scoped_counts_stay_inside_their_graph() {
    for direct in [false, true] {
        let mut b = fixture(direct);
        execute_update(
            "INSERT DATA { GRAPH <http://ex/g> { \
               <http://ex/a> <http://ex/p0> <http://ex/o> . \
               <http://ex/b> <http://ex/p0> <http://ex/o> } }",
            &mut b,
        )
        .unwrap();

        let in_graph =
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <http://ex/g> { ?s <http://ex/p0> <http://ex/o> } }";
        assert_eq!(count(&b, in_graph), 2, "direct={direct}");
        assert_eq!(
            count(&b, COUNT_PO) as usize,
            rows(&b, "SELECT ?s WHERE { ?s <http://ex/p0> <http://ex/o> }"),
            "direct={direct}: default-graph count must match its own scan"
        );
    }
}
