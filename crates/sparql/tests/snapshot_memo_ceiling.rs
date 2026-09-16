//! HDB-231 / SPEC-31 S6: the store-side ceiling on the snapshot memo.
//!
//! The memoised `VecTripleSource` is built by the first query on a commit
//! version, kept for the life of that version, and reused by every later
//! query. `[server.limits].max_snapshot_memory` bounds it: a scope whose
//! worst-case snapshot would push the memo past the ceiling is refused
//! before it is built, with `SparqlError::SnapshotMemoryLimit`.
//!
//! These tests pin the three things the behaviour has to get right: a build
//! over the ceiling is refused, a build under it is served, and a memo *hit*
//! is never refused no matter where the ceiling sits.

use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::error::SparqlError;
use horndb_sparql::exec::horn::HornBackend;

/// Rows per predicate. Small enough to stay a laptop test, large enough that
/// the memo is not rounding error.
const ROWS: u64 = 2_000;
const PREDICATES: u64 = 2;
const TRIPLES: u64 = ROWS * PREDICATES;

/// Worst-case memo bytes per triple: six orderings x three `TermId` columns
/// x 8 B. Kept in step with `HornBackend`'s own constant by the assertions
/// below — if the engine's estimate changes, the `just_fits` case fails.
const BYTES_PER_TRIPLE: u64 = 144;

fn iri(v: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v))
}

/// A backend on the memoised read path — `set_direct_source(false)` pins it
/// regardless of `HORNDB_DIRECT_SOURCE`, since the direct source never
/// touches the memo and would make every case here vacuous.
fn fixture(ceiling: Option<u64>) -> HornBackend {
    let mut b = HornBackend::new();
    b.set_direct_source(false);
    b.set_max_snapshot_memory(ceiling);
    let mut triples = Vec::new();
    for p in 0..PREDICATES {
        for i in 0..ROWS {
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

const SCAN: &str = "SELECT ?s ?o WHERE { ?s <http://ex/p0> ?o }";

fn rows(b: &HornBackend) -> usize {
    match execute_query(SCAN, b).unwrap() {
        QueryAnswer::Solutions { rows, .. } => rows.len(),
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Unbounded is still expressible, and is what an embedded caller that never
/// set a ceiling gets.
#[test]
fn no_ceiling_serves_and_memoises() {
    let b = fixture(None);
    assert_eq!(rows(&b), ROWS as usize);
    assert!(
        b.snapshot_memo_bytes() > 0,
        "the memoised path must have built a snapshot"
    );
}

/// A ceiling one byte under the worst case refuses the query — and builds
/// nothing, so the memo is exactly what it was.
#[test]
fn ceiling_below_one_whole_scope_ordering_refuses_the_query() {
    let worst_case = TRIPLES * BYTES_PER_TRIPLE;
    let b = fixture(Some(worst_case - 1));
    let err = execute_query(SCAN, &b).expect_err("the build must be refused");
    match err {
        SparqlError::SnapshotMemoryLimit {
            limit,
            held,
            requested,
        } => {
            assert_eq!(limit, worst_case - 1);
            assert_eq!(held, 0, "nothing was memoised before this query");
            assert_eq!(requested, worst_case);
        }
        other => panic!("expected SnapshotMemoryLimit, got {other:?}"),
    }
    assert_eq!(
        b.snapshot_memo_bytes(),
        0,
        "a refused build must leave the memo untouched"
    );
}

/// The refusal is the ceiling, not a blanket failure: the same store at the
/// exact worst-case ceiling serves the query.
#[test]
fn ceiling_at_the_worst_case_just_fits() {
    let b = fixture(Some(TRIPLES * BYTES_PER_TRIPLE));
    assert_eq!(rows(&b), ROWS as usize);
}

/// A memo hit is never refused. Lowering the ceiling below what the memo
/// already holds stops *new* builds; it must not start failing queries the
/// store already has the snapshot for, or the answer to a query would depend
/// on when the operator edited the config.
#[test]
fn a_warm_scope_is_served_however_low_the_ceiling_goes() {
    let mut b = fixture(None);
    assert_eq!(rows(&b), ROWS as usize);
    let warm = b.snapshot_memo_bytes();
    assert!(warm > 0);

    b.set_max_snapshot_memory(Some(1));
    assert_eq!(
        rows(&b),
        ROWS as usize,
        "a hit must not consult the ceiling"
    );
    assert_eq!(b.snapshot_memo_bytes(), warm, "and must not rebuild");
}

/// The gauge the ceiling is read against is the one `memory_split` reports,
/// so an operator watching `horndb_sparql_snapshot_memo_bytes` sees the same
/// number the refusal is decided on.
#[test]
fn the_gauge_and_the_ceiling_read_the_same_number() {
    let b = fixture(None);
    assert_eq!(rows(&b), ROWS as usize);
    assert_eq!(b.snapshot_memo_bytes(), b.memory_split().snapshots);
    assert_eq!(
        b.snapshot_memo_bytes(),
        b.storage_stats().snapshot_memo_bytes
    );
}
