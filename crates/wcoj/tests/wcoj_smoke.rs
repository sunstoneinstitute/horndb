use arrow::array::UInt64Array;
use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::ExecutionPlan;
use horndb_wcoj::source::vec_source::VecTripleSource;

fn collect_pairs(batches: Vec<arrow::record_batch::RecordBatch>) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = Vec::new();
    for b in batches {
        let n = b.num_rows();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..n {
            out.push(cols.iter().map(|c| c.value(r)).collect());
        }
    }
    out
}

#[test]
fn triangle_join_produces_correct_results() {
    // Triangle: (?a, p, ?b)(?b, p, ?c)(?c, p, ?a) over a tiny graph.
    // Edges: 1→2, 2→3, 3→1 (forms a triangle), plus noise 1→4.
    let p = 10;
    let triples = vec![
        Triple::new(1, p, 2),
        Triple::new(2, p, 3),
        Triple::new(3, p, 1),
        Triple::new(1, p, 4),
    ];
    let src = VecTripleSource::from_triples(triples);

    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1), Var(2)],
    };

    let cancel = CancelToken::new();
    let exec = WcojExecutor::new(&src, &bgp, &plan, cancel);
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    let mut rows = collect_pairs(batches);
    rows.sort();
    // Triangles: (1,2,3), (2,3,1), (3,1,2) are the same cycle viewed from
    // each starting vertex — the join produces all three.
    assert_eq!(rows, vec![vec![1, 2, 3], vec![2, 3, 1], vec![3, 1, 2]]);
}

#[test]
fn empty_result_yields_no_batches() {
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(99), Term::Var(Var(1))),
    ]);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1)],
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(collect_pairs(batches).len(), 0);
}

/// Regression: leapfrog at depth `d` over three iters with initial peeks
/// [A=14, B=14, C=2] (no two adjacent in `contributing[d]` order share a
/// key with all the others) used to falsely emit 14 — `find_match` only
/// compared `iter[p]` against `iter[(p+k-1) % k]`, never against the
/// remaining iters in the cycle. The fix sorts `contributing[d]` by peek
/// on prime, restoring the classic Veldhuizen leapfrog invariant. See
/// `differential_fuzz.proptest-regressions` (deleted) for the proptest
/// shrunk seed that originally surfaced this.
#[test]
fn three_way_leapfrog_at_depth_does_not_over_match() {
    let p = 100u64;
    // (?V1, p, ?V0) over edges making P0 enumerate V0 ∈ {14}: only
    //   subject 25 has (25, p, 14).
    // (26, p, ?V0): subject-26 has p with V0 ∈ {14}.
    // (2,  q, ?V0): subject-2  has q (≠ p) with V0 ∈ {2}.
    // The three-way intersection at V0 must be empty (14 ∉ {2}).
    let q = 200u64;
    let triples = vec![
        // (?V1, p, V0) candidates with V0 == 14:
        Triple::new(25, p, 14),
        Triple::new(26, p, 14),
        // (26, p, V0):
        // already includes (26, p, 14).
        // (2, q, V0):
        Triple::new(2, q, 2),
    ];
    let src = VecTripleSource::from_triples(triples);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(0))),
        TriplePattern::new(Term::Bound(26), Term::Bound(p), Term::Var(Var(0))),
        TriplePattern::new(Term::Bound(2), Term::Bound(q), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(1), Var(0)],
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    let rows = collect_pairs(batches);
    assert!(
        rows.is_empty(),
        "leapfrog over-produced rows: {rows:?} (V0=14 should not match P2 which only has V0=2)"
    );
}

/// Regression: even when only two iters contribute at a depth, the
/// pre-fix leapfrog could mis-prime — it set `p = 0` unconditionally so
/// `target = iter[prev=k-1].peek` was whichever iter happened to land last
/// in `contributing` order, not the maximum. Sorting on prime guarantees
/// `target` is the global max regardless of input order.
#[test]
fn two_way_leapfrog_handles_swapped_initial_order() {
    let p = 100u64;
    // Two patterns that share V0; only V0=5 is in both.
    let triples = vec![
        // (V1, p, V0) candidates from S=10: V0 ∈ {3, 5, 7}
        Triple::new(10, p, 3),
        Triple::new(10, p, 5),
        Triple::new(10, p, 7),
        // (20, p, V0): V0 ∈ {2, 5, 9}
        Triple::new(20, p, 2),
        Triple::new(20, p, 5),
        Triple::new(20, p, 9),
    ];
    let src = VecTripleSource::from_triples(triples);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(0))),
        TriplePattern::new(Term::Bound(20), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(1), Var(0)],
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    let mut rows = collect_pairs(batches);
    rows.sort();
    // P0 enumerates (V1, V0) ∈ {(10,3), (10,5), (10,7), (20,2), (20,5), (20,9)};
    // P1 restricts V0 ∈ {2, 5, 9}. Join keeps:
    //   (10, 5), (20, 2), (20, 5), (20, 9).
    assert_eq!(
        rows,
        vec![vec![10, 5], vec![20, 2], vec![20, 5], vec![20, 9]]
    );
}
