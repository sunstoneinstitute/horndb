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
