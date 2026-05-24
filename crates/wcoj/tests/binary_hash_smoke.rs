use arrow::array::UInt64Array;
use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::binary_hash::BinaryHashExecutor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::source::vec_source::VecTripleSource;

fn collect(batches: Vec<arrow::record_batch::RecordBatch>) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = Vec::new();
    for b in batches {
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.push(cols.iter().map(|c| c.value(r)).collect());
        }
    }
    out
}

#[test]
fn binary_hash_join_triangle() {
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
    let exec =
        BinaryHashExecutor::new(&src, &bgp, vec![Var(0), Var(1), Var(2)], CancelToken::new());
    let mut rows = collect(exec.into_iter().collect::<Result<_, _>>().unwrap());
    rows.sort();
    assert_eq!(rows, vec![vec![1, 2, 3], vec![2, 3, 1], vec![3, 1, 2]]);
}

#[test]
fn binary_hash_join_ground_pattern_returns_one_empty_row_when_match() {
    // (1, 10, 2) is in the graph — match yields one empty binding.
    let src = VecTripleSource::from_triples(vec![Triple::new(1, 10, 2)]);
    let bgp = Bgp::new(vec![TriplePattern::new(
        Term::Bound(1),
        Term::Bound(10),
        Term::Bound(2),
    )]);
    let exec = BinaryHashExecutor::new(&src, &bgp, vec![], CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
}
