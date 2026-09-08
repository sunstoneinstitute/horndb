//! SPEC-31 AC6 and AC7: the query memory budget charges blocking operators
//! in proportion to what they hold, and a store-side fast path (the
//! single-predicate `COUNT`, HDB-229) charges the query nothing at all.
//!
//! `budget::scope` is thread-local and `execute_query` runs synchronously on
//! the calling thread, so a scope installed here *is* the query's scope:
//! `budget::peak()` is readable right after `execute_query` returns, before
//! the guard drops.

use horndb_sparql::api::{execute_query, plan_select, QueryAnswer};
use horndb_sparql::error::SparqlError;
use horndb_sparql::exec::budget;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::plan::PhysicalPlan;
use horndb_sparql::SparqlConfig;

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

/// `n` triples, each with its own predicate:
/// `<http://ex/s{i}> <http://ex/p{i}> <http://ex/o{i}>`. Unlike `store()`,
/// `GROUP BY ?p` over this data does not collapse to one group — every row
/// is its own group, so the `Group` output is as wide as its input. That
/// matters for `stacked_blocking_operators_charge_the_sum_not_the_max`: an
/// `OrderBy` stacked on top must charge a real, comparable amount, not a
/// single row.
fn store_unique_predicates(n: u64) -> HornBackend {
    let mut b = HornBackend::new();
    let triples = (0..n)
        .map(|i| {
            (
                iri(&format!("http://ex/s{i}")),
                iri(&format!("http://ex/p{i}")),
                iri(&format!("http://ex/o{i}")),
            )
        })
        .collect();
    b.insert_oxrdf_batch(triples).unwrap();
    b
}

const GROUP_THEN_ORDER: &str =
    "SELECT ?p (MAX(?o) AS ?m) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY ?p";

fn run_group_then_order(b: &HornBackend) -> Result<QueryAnswer, SparqlError> {
    execute_query(GROUP_THEN_ORDER, b)
}

/// Walks a `PhysicalPlan`, reporting whether a `Group` and an `OrderBy` node
/// are both present anywhere in the tree — used to confirm the query below
/// really does lower to two stacked blocking operators, not one fused into
/// the other or optimized away.
fn plan_has_group_and_orderby(plan: &PhysicalPlan) -> (bool, bool) {
    fn walk(plan: &PhysicalPlan, has_group: &mut bool, has_orderby: &mut bool) {
        use PhysicalPlan::*;
        match plan {
            Group { inner, .. } => {
                *has_group = true;
                walk(inner, has_group, has_orderby);
            }
            OrderBy { inner, .. } => {
                *has_orderby = true;
                walk(inner, has_group, has_orderby);
            }
            Join { left, right }
            | LeftJoin { left, right, .. }
            | Minus { left, right }
            | Union { left, right } => {
                walk(left, has_group, has_orderby);
                walk(right, has_group, has_orderby);
            }
            Filter { inner, .. }
            | Project { inner, .. }
            | Distinct { inner }
            | Slice { inner, .. }
            | Extend { inner, .. }
            | PerGraph { inner, .. } => walk(inner, has_group, has_orderby),
            PathClosure { edge, .. } => walk(edge, has_group, has_orderby),
            BgpScan { .. } | CountScan { .. } | GroupCountScan { .. } | Values { .. } => {}
        }
    }
    let mut has_group = false;
    let mut has_orderby = false;
    walk(plan, &mut has_group, &mut has_orderby);
    (has_group, has_orderby)
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

/// SPEC-31 M1: `budget::peak()` is a high-water mark, so a `Reservation`
/// dropped early (e.g. right after its operator's own `drain` call, instead
/// of when the operator itself drops) can still leave the smaller of two
/// stacked charges as the observed peak, and every other test in this file
/// would stay green. `GROUP BY ?p ... ORDER BY ?p` stacks a `GroupOp` under
/// an `OrderByOp`: `OrderByOp::next` calls `drain` on its child, which pulls
/// the still-live `GroupOp` to exhaustion without dropping it, so both
/// reservations are held at once at the moment `OrderBy` finishes charging
/// its own (aggregated) input. The combined peak must reflect both charges
/// summed, not just the larger one.
#[test]
fn stacked_blocking_operators_charge_the_sum_not_the_max() {
    let cfg = SparqlConfig::default();
    let (_, plan, _) = plan_select(GROUP_THEN_ORDER, &cfg)
        .unwrap()
        .expect("a plain SELECT plans to Some((vars, plan, dataset))");
    let (has_group, has_orderby) = plan_has_group_and_orderby(&plan);
    assert!(
        has_group,
        "GROUP_THEN_ORDER must lower to a plan containing a Group node: {plan:?}"
    );
    assert!(
        has_orderby,
        "GROUP_THEN_ORDER must lower to a plan containing an OrderBy node: {plan:?}"
    );

    // Unique-per-row predicates: GROUP BY ?p does not collapse the rows, so
    // OrderBy's own charge (over Group's output) is comparable in size to
    // Group's charge (over the raw scan), not a single leftover row.
    let s = store_unique_predicates(4_000);

    let group_only_peak = {
        let _g = budget::scope(None);
        run_group_by(&s).unwrap();
        budget::peak()
    };

    let stacked_peak = {
        let _g = budget::scope(None);
        run_group_then_order(&s).unwrap();
        budget::peak()
    };

    assert!(
        group_only_peak > 0,
        "GROUP BY alone must charge something for 4,000 rows"
    );
    // Measured ~2.1x on this data; the RED check (dropping GroupOp's
    // Reservation right after drain instead of holding it for the
    // operator's life) measured 1.10x, so 1.3x leaves headroom on both
    // sides without being so tight it flakes.
    assert!(
        stacked_peak as f64 > group_only_peak as f64 * 1.3,
        "stacked peak={stacked_peak} must exceed the GROUP-BY-only peak={group_only_peak} \
         by a wide margin — both operators' charges must be live at once, not just the \
         larger one (a Reservation dropped right after its operator's drain call \
         measured 1.10x here, not 2.1x)"
    );
}
