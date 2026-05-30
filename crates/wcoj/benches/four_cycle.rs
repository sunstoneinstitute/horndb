//! SPEC-03 acceptance criterion #2: on the 4-cycle query
//!   (?a -p-> ?b -p-> ?c -p-> ?d -p-> ?a)
//! over a synthetic graph with ~10^6 edges, WCOJ outperforms binary-hash
//! by ≥10×.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::compressed::CompressedTripleSource;
use horndb_wcoj::source::synthetic::SyntheticGraph;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::source::TripleSource;

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
    let edges = SyntheticGraph::cyclic_edges(250_000, 4, 10, 0xDEAD_BEEF);
    let dense = VecTripleSource::from_triples(edges.clone());
    let compressed = CompressedTripleSource::from_triples(edges);
    let bgp = make_4_cycle_bgp();

    // One-time footprint report (stdout; criterion does not capture this).
    let comp_bytes = compressed.heap_bytes();
    let n = dense.total_triples().max(1);
    eprintln!(
        "four_cycle source footprint: compressed = {} bytes ({:.2} B/triple over 6 orderings); \
         dense ≈ {} bytes ({} B/triple)",
        comp_bytes,
        comp_bytes as f64 / n as f64,
        n * 6 * 24,
        6 * 24,
    );

    let mut group = c.benchmark_group("four_cycle");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("wcoj_dense", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&dense, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("wcoj_compressed", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&compressed, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash_dense", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &dense,
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

    group.bench_function("binary_hash_compressed", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &compressed,
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
