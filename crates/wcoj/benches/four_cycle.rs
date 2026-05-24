//! SPEC-03 acceptance criterion #2: on the 4-cycle query
//!   (?a -p-> ?b -p-> ?c -p-> ?d -p-> ?a)
//! over a synthetic graph with ~10^6 edges, WCOJ outperforms binary-hash
//! by ≥10×.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::executor::binary_hash::BinaryHashExecutor;
use reasoner_wcoj::executor::wcoj::WcojExecutor;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::plan::{ExecutionPlan, PlanKind};
use reasoner_wcoj::source::synthetic::SyntheticGraph;

fn make_4_cycle_bgp() -> Bgp {
    let p = 10u64;
    Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ])
}

fn bench_four_cycle(c: &mut Criterion) {
    // 10^6 edges: 250_000 vertices * 4 out-edges = 1_000_000.
    let graph = SyntheticGraph::cyclic(250_000, 4, 10, 0xDEAD_BEEF);
    let bgp = make_4_cycle_bgp();

    let mut group = c.benchmark_group("four_cycle");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("wcoj", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&graph, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &graph,
                &bgp,
                vec![Var(0), Var(1), Var(2), Var(3)],
                CancelToken::new(),
            );
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_four_cycle);
criterion_main!(benches);
