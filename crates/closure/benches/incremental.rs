//! SPEC-05 F6 bench: incremental single-edge insert vs full GraphBLAS
//! recompute on a transitivity chain.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

/// Edges of a chain 0->1->2->...->(n-1), plus its transitive closure size.
fn chain(n: u64) -> Vec<(u64, u64)> {
    (0..n - 1).map(|i| (i, i + 1)).collect()
}

fn bench_incremental_vs_full(c: &mut Criterion) {
    init_once().unwrap();
    let n: u64 = 2_000;
    let base = chain(n); // 0..n-1 chain
    let new_edge = (n - 1, n); // appends one node, extending the chain

    let mut group = c.benchmark_group("spec05_incremental_append");

    // Full recompute: build matrix of base+new_edge and close from scratch.
    group.bench_function("full_recompute", |b| {
        let mut all = base.clone();
        all.push(new_edge);
        b.iter(|| {
            let m = BoolMatrix::from_edges(n + 1, &all).unwrap();
            let star = transitive_closure(&m).unwrap();
            black_box(star.nvals().unwrap());
        });
    });

    // Incremental: pre-close the base once, then time only the single insert.
    group.bench_function("incremental_insert", |b| {
        let m = BoolMatrix::from_edges(n + 1, &base).unwrap();
        let closed = transitive_closure(&m).unwrap().extract_edges().unwrap();
        b.iter_batched(
            || IncrementalTransitiveClosure::from_closed_edges(closed.iter().copied()),
            |mut inc| {
                let delta = inc.insert_edge(new_edge.0, new_edge.1);
                black_box(delta.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_incremental_vs_full);
criterion_main!(benches);
