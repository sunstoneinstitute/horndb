//! Bench: SPEC-05 acceptance criterion 1.
//!
//! "Transitivity-chain benchmark of 2,500 nodes: faster than RDFox by 10×
//!  and faster than GraphDB/OWLIM by 50×."
//!
//! Stage-1 reduced goal: simply finish, and demonstrate the closure is
//! faster than the naive rule-firing baseline (the rule engine does not
//! exist yet in Stage 1, so we measure absolute throughput here and gate
//! the comparative claim in Stage 2).

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn chain_matrix(n: u64) -> BoolMatrix {
    let edges: Vec<(u64, u64)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    BoolMatrix::from_edges(n, &edges).unwrap()
}

fn bench_transitive_chain(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("transitive_chain");
    group.measurement_time(Duration::from_secs(20));

    for &n in &[100u64, 500, 2_500] {
        // Closure of an n-chain produces n*(n-1)/2 edges.
        group.throughput(Throughput::Elements(n * (n - 1) / 2));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let m = chain_matrix(n);
            b.iter(|| {
                let star = transitive_closure(&m).unwrap();
                assert_eq!(star.nvals().unwrap(), n * (n - 1) / 2);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_transitive_chain);
criterion_main!(benches);
