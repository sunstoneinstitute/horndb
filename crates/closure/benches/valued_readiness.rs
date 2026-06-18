//! Bench: valued-reasoning readiness metrics (TASKS.md #11).
//!
//! These benches produce the numbers the #11 decision rule needs *before* any
//! custom-semiring work (#12) is attempted. Two questions:
//!
//! 1. **Valued vs. boolean kernel split** — how much more expensive is a valued
//!    `(max, ×)` `GrB_mxm` than the Boolean `(∨, ∧)` reachability baseline on
//!    the *same* matrix shape? This is the price of carrying a scalar
//!    confidence at all.
//! 2. **Generic-kernel penalty** — how much slower is the *same* `(max, ×)`
//!    closure when SuiteSparse must use its generic kernel (user-defined op)
//!    instead of the prepackaged FactoryKernel? This multiplier is exactly what
//!    JIT/PreJIT would remove (#12 Fork B / PreJIT).
//!
//! Run: `cargo bench -p horndb-closure --bench valued_readiness`.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix, ValuedMatrix};
use horndb_closure::metrics::{valued_transitive_closure, ValuedKernel};

/// Build an n-chain `0 -> 1 -> … -> n-1` in both Boolean and valued forms,
/// using the same shape so the comparison is apples-to-apples.
fn chain_bool(n: u64) -> BoolMatrix {
    let edges: Vec<(u64, u64)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    BoolMatrix::from_edges(n, &edges).unwrap()
}

fn chain_valued(n: u64) -> ValuedMatrix {
    // Weights in (0, 1] so `(max, ×)` stays a contraction (monotone), giving a
    // finite fixed point identical in *support* to the Boolean closure.
    let edges: Vec<(u64, u64, f64)> = (0..n - 1).map(|i| (i, i + 1, 0.99)).collect();
    ValuedMatrix::from_weighted_edges(n, &edges).unwrap()
}

/// (1) Valued `(max, ×)` closure vs Boolean `(∨, ∧)` closure on the same chain.
fn bench_valued_vs_boolean(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("valued_vs_boolean_closure");
    group.measurement_time(Duration::from_secs(12));

    for &n in &[500u64, 2_500] {
        let pairs = n * (n - 1) / 2;
        group.throughput(Throughput::Elements(pairs));

        group.bench_with_input(BenchmarkId::new("boolean", n), &n, |b, &n| {
            let m = chain_bool(n);
            b.iter(|| {
                let star = transitive_closure(&m).unwrap();
                assert_eq!(star.nvals().unwrap(), pairs);
            });
        });

        group.bench_with_input(BenchmarkId::new("valued_builtin", n), &n, |b, &n| {
            let m = chain_valued(n);
            b.iter(|| {
                let (star, _metrics) =
                    valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
                assert_eq!(star.nvals().unwrap(), pairs);
            });
        });
    }
    group.finish();
}

/// (2) Generic-kernel penalty: built-in FactoryKernel vs user-defined-op
/// generic kernel for the *same* valued `(max, ×)` closure.
fn bench_generic_kernel_penalty(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("generic_kernel_penalty");
    group.measurement_time(Duration::from_secs(12));

    for &n in &[500u64, 2_500] {
        let pairs = n * (n - 1) / 2;
        group.throughput(Throughput::Elements(pairs));

        group.bench_with_input(BenchmarkId::new("builtin", n), &n, |b, &n| {
            let m = chain_valued(n);
            b.iter(|| {
                let (star, _m) = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
                assert_eq!(star.nvals().unwrap(), pairs);
            });
        });

        group.bench_with_input(BenchmarkId::new("udf_generic", n), &n, |b, &n| {
            let m = chain_valued(n);
            b.iter(|| {
                let (star, _m) = valued_transitive_closure(&m, ValuedKernel::Udf).unwrap();
                assert_eq!(star.nvals().unwrap(), pairs);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_valued_vs_boolean,
    bench_generic_kernel_penalty
);
criterion_main!(benches);
