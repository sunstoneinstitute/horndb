//! SPEC-24 S2 A/B: output-sensitive support-counting deletion vs the recompute
//! fallback, for a small-delta delete over a growing store. Run on hornbench for
//! recorded numbers (see docs/benchmarks.md); local runs are smoke only.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_closure::closure::incremental::{DeleteStrategy, IncrementalTransitiveClosure};

fn build(n_chains: u64) -> IncrementalTransitiveClosure {
    let mut c = IncrementalTransitiveClosure::new();
    for i in 0..n_chains {
        let a = i * 10;
        let b = i * 10 + 1;
        let d = i * 10 + 2;
        c.insert_edge(a, b);
        c.insert_edge(b, d);
    }
    c.insert_edge(0, 2); // redundant edge on chain 0
    c
}

fn bench_delete(cr: &mut Criterion) {
    let mut group = cr.benchmark_group("closure_retraction_redundant_delete");
    for &n in &[100u64, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("support_counting", n), &n, |bch, &n| {
            bch.iter_batched(
                || {
                    let mut c = build(n);
                    c.set_delete_strategy(DeleteStrategy::SupportCounting);
                    c
                },
                |mut c| {
                    let _ = c.delete_edge(0, 2);
                    c
                },
                criterion::BatchSize::SmallInput,
            );
        });
        group.bench_with_input(BenchmarkId::new("recompute", n), &n, |bch, &n| {
            bch.iter_batched(
                || {
                    let mut c = build(n);
                    c.set_delete_strategy(DeleteStrategy::Recompute);
                    c
                },
                |mut c| {
                    let _ = c.delete_edge(0, 2);
                    c
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_delete);
criterion_main!(benches);
