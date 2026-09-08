//! SPEC-31 AC6 and AC7: the query memory budget charges blocking operators
//! in proportion to what they hold, and a store-side fast path (the
//! single-predicate `COUNT`, HDB-229) charges the query nothing at all.
//!
//! `budget::scope` is thread-local and `execute_query` runs synchronously on
//! the calling thread, so a scope installed here *is* the query's scope:
//! `budget::peak()` is readable right after `execute_query` returns, before
//! the guard drops.

use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::error::SparqlError;
use horndb_sparql::exec::budget;
use horndb_sparql::exec::horn::HornBackend;

fn iri(v: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v))
}

/// `n` triples `<http://ex/s{i}> <http://ex/p> <http://ex/o{i}>`, batched
/// (per-triple `insert_oxrdf` is too slow for thousands of rows in debug).
fn store(n: u64) -> HornBackend {
    let mut b = HornBackend::new();
    let triples = (0..n)
        .map(|i| {
            (
                iri(&format!("http://ex/s{i}")),
                iri("http://ex/p"),
                iri(&format!("http://ex/o{i}")),
            )
        })
        .collect();
    b.insert_oxrdf_batch(triples).unwrap();
    b
}

/// `MAX` is not a plain count (`plan/pushdown.rs::is_plain_count` is false
/// for it), so this stays on `GroupOp` instead of the `CountScan` pushdown —
/// the shape that actually charges the budget.
const GROUP_BY_MAX: &str = "SELECT ?p (MAX(?o) AS ?m) WHERE { ?s ?p ?o } GROUP BY ?p";

fn run_group_by(b: &HornBackend) -> Result<QueryAnswer, SparqlError> {
    execute_query(GROUP_BY_MAX, b)
}

/// AC6, first half: the charge is proportional to what the operator holds.
/// `drain` charges each chunk as `len * size_of::<Row>() + Σ heap_bytes`,
/// linear in row count, so a 10x larger input must produce a peak in the
/// same order of magnitude, not a flat or unrelated number. The 8x-12x
/// window (rather than exactly 10x) absorbs the unlikely case that the
/// planner projects a different row width for the two sizes.
#[test]
fn group_by_charge_grows_with_the_input() {
    let small_store = store(2_000);
    let large_store = store(20_000);

    let peak_small = {
        let _g = budget::scope(None);
        run_group_by(&small_store).unwrap();
        budget::peak()
    };
    let peak_large = {
        let _g = budget::scope(None);
        run_group_by(&large_store).unwrap();
        budget::peak()
    };

    assert!(
        peak_small > 0,
        "GROUP BY must charge something for 2,000 rows"
    );
    assert!(
        peak_large >= 8 * peak_small && peak_large <= 12 * peak_small,
        "peak(20_000)={peak_large} must be 8x-12x peak(2_000)={peak_small}"
    );
}

/// AC6, second half: a ceiling set between the two charges admits the
/// smaller query and refuses the larger one with `QueryMemoryLimit`, and a
/// refused query leaves nothing charged behind (AC3 — the dropped operator
/// tree and rejected charge cancel out).
#[test]
fn a_ceiling_between_two_sizes_admits_the_small_and_refuses_the_large() {
    let small_store = store(2_000);
    let large_store = store(20_000);

    let small = {
        let _g = budget::scope(None);
        run_group_by(&small_store).unwrap();
        budget::peak()
    };
    let ceiling = small * 2;

    {
        let _g = budget::scope(Some(ceiling));
        run_group_by(&small_store).expect("the small query fits under the ceiling");
    }

    {
        let _g = budget::scope(Some(ceiling));
        let err = run_group_by(&large_store).expect_err("the large query must be refused");
        match err {
            SparqlError::QueryMemoryLimit { limit, requested } => {
                assert_eq!(limit, ceiling);
                assert!(
                    requested > limit,
                    "requested={requested} must exceed limit={limit}"
                );
            }
            other => panic!("expected QueryMemoryLimit, got {other:?}"),
        }
        assert_eq!(
            budget::used(),
            0,
            "a rejected charge and a dropped operator tree must leave nothing behind"
        );
    }
}

/// AC7: the single-predicate `COUNT` that motivated this spec (HDB-229)
/// answers off a store-side fast path, not `GroupOp`, so it charges the
/// *query* budget zero even though it grows the memoised snapshot the
/// backend keeps for the direct source. A one-byte budget must not refuse
/// it — the memo's growth is a store-side cost (HDB-231), separate from
/// this budget.
#[test]
fn the_pushdown_count_charges_nothing_to_the_query() {
    const HOT: u64 = 100;
    const COLD: u64 = 20_000;
    const PREDICATES: u64 = 2;

    let mut b = HornBackend::new();
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

    let before = b.memory_split().snapshots;
    let (result, peak) = {
        let _g = budget::scope(Some(1));
        let result = execute_query(
            "SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/p0> <http://ex/o> }",
            &b,
        );
        (result, budget::peak())
    };
    let after = b.memory_split().snapshots;

    match result.unwrap() {
        QueryAnswer::Solutions { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let Some(horndb_sparql::algebra::Term::Literal(lit)) = rows[0].get("n") else {
                panic!("?n must be a literal, got {:?}", rows[0].get("n"))
            };
            let count: u64 = lit
                .trim_start_matches('"')
                .split('"')
                .next()
                .expect("literal lexical form")
                .parse()
                .expect("integer count");
            assert_eq!(count, HOT);
        }
        other => panic!("expected solutions, got {other:?}"),
    }
    assert_eq!(
        peak, 0,
        "the pushdown count must not charge the query budget"
    );
    assert!(
        after > before,
        "the store-side memo must still grow (HDB-231): before={before}, after={after}"
    );
}
