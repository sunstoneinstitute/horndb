//! Coverage for the `horndb_simd::intersect` k==2 fast path wired into the
//! production `BatchIter` (`executor::wcoj`). The differential fuzzer uses
//! `N_VERTICES = 30` (< `SIMD_SEEK_MIN_RUN = 64`), so it never materialises an
//! `active_run` and never arms SIMD. These tests deliberately drive ≥64-wide
//! depth-0 k==2 intersections so the SIMD path engages, and compare WCOJ output
//! against `BinaryHashExecutor` as the oracle.

use std::collections::BTreeSet;

use arrow::array::UInt64Array;

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::vec_source::VecTripleSource;

fn collect_rows(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
) -> BTreeSet<Vec<TermId>> {
    let mut out = BTreeSet::new();
    for b in batches {
        let b = b.unwrap();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.insert(cols.iter().map(|c| c.value(r)).collect::<Vec<TermId>>());
        }
    }
    out
}

fn total_rows(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
) -> usize {
    batches.map(|b| b.unwrap().num_rows()).sum()
}

fn run_both(src: &VecTripleSource, bgp: &Bgp, var_order: Vec<Var>) -> BTreeSet<Vec<TermId>> {
    let plan = ExecutionPlan {
        kind: PlanKind::Wcoj,
        var_order: var_order.clone(),
    };
    let wcoj_rows =
        collect_rows(WcojExecutor::new(src, bgp, &plan, CancelToken::new()).into_iter());
    let bh_rows = collect_rows(
        BinaryHashExecutor::new(src, bgp, var_order.clone(), CancelToken::new()).into_iter(),
    );
    assert_eq!(
        wcoj_rows, bh_rows,
        "WCOJ output must match BinaryHash oracle"
    );

    // Compare RAW emitted-row counts too, not just the deduped set: a SIMD
    // intersect fed a non-deduped `active_run` would over-produce identical
    // rows that a set comparison silently collapses. The two executors must
    // emit the same number of rows.
    let plan = ExecutionPlan {
        kind: PlanKind::Wcoj,
        var_order: var_order.clone(),
    };
    let wcoj_n = total_rows(WcojExecutor::new(src, bgp, &plan, CancelToken::new()).into_iter());
    let bh_n =
        total_rows(BinaryHashExecutor::new(src, bgp, var_order, CancelToken::new()).into_iter());
    assert_eq!(
        wcoj_n, bh_n,
        "WCOJ emitted {wcoj_n} rows but BinaryHash emitted {bh_n} (over-production?)"
    );

    wcoj_rows
}

/// (a) Single-variable wide case. Patterns `?x 10 1` and `?x 20 1` over a
/// ≥64-wide shared ?x run force `active_run` to materialise a SoA column at
/// depth 0, so the k==2 SIMD intersect path engages. Output must equal A ∩ B.
#[test]
fn wcoj_k2_simd_single_var_wide() {
    let a: Vec<u64> = (0..120u64).collect();
    let b: Vec<u64> = (60..200u64).step_by(2).collect();

    let mut triples = Vec::new();
    for &s in &a {
        triples.push(Triple::new(s, 10, 1));
    }
    for &s in &b {
        triples.push(Triple::new(s, 20, 1));
    }
    let src = VecTripleSource::from_triples(triples);

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1));
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Bound(1));
    let bgp = Bgp::new(vec![p1, p2]);

    let rows = run_both(&src, &bgp, vec![Var(0)]);

    let set_a: BTreeSet<u64> = a.into_iter().collect();
    let set_b: BTreeSet<u64> = b.into_iter().collect();
    let expected: BTreeSet<Vec<TermId>> = set_a.intersection(&set_b).map(|&x| vec![x]).collect();
    assert_eq!(rows, expected);
    // The overlap is wide enough to have exercised the SoA/SIMD path, not a
    // degenerate short run.
    assert!(
        rows.len() >= 20,
        "expected a wide overlap, got {}",
        rows.len()
    );
}

/// (b) Multi-variable case WITH DESCENT. Patterns `?x 10 ?y` and `?x 20 ?y`,
/// var_order `[?x, ?y]`. The shared ?x run is ≥64 wide (engages depth-0 SIMD),
/// and every shared x carries several y values under BOTH predicates, so the
/// executor descends to depth 1 between SIMD emissions and must resume draining
/// the depth-0 SIMD buffer correctly after ascending. This validates that the
/// per-depth SIMD buffer survives descend/ascend.
#[test]
fn wcoj_k2_simd_multi_var_with_descent() {
    // 100 distinct shared x (>= 64 → depth-0 SIMD arms). Per x:
    //   pred 10: y in {1,2,3,4,5}
    //   pred 20: y in {3,4,5,6,7}
    // The (x,y) join result is { (x, y) : x in 0..100, y in {3,4,5} }.
    let xs: Vec<u64> = (0..100u64).collect();
    let mut triples = Vec::new();
    for &x in &xs {
        for y in 1..=5u64 {
            triples.push(Triple::new(x, 10, y));
        }
        for y in 3..=7u64 {
            triples.push(Triple::new(x, 20, y));
        }
    }
    let src = VecTripleSource::from_triples(triples);

    let p1 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1)));
    let p2 = TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Var(Var(1)));
    let bgp = Bgp::new(vec![p1, p2]);

    let rows = run_both(&src, &bgp, vec![Var(0), Var(1)]);

    let expected: BTreeSet<Vec<TermId>> = xs
        .iter()
        .flat_map(|&x| (3..=5u64).map(move |y| vec![x, y]))
        .collect();
    assert_eq!(rows, expected);
    assert_eq!(rows.len(), 100 * 3);
}
