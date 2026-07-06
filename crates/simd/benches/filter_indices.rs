//! SPEC-12: `filter_indices_eq` (scan + index-compact) SIMD-over-scalar
//! speedup on L2-resident u64 columns (AVX2 hand-written kernel). Benched at
//! two selectivities — sparse (~1% match) and dense (~50% match) — because the
//! match density drives the set-bit-extraction / compaction cost that dominates
//! once the compare is vectorized. Throughput is counted in scanned elements.
//! Run on hornbench; record the ratios in docs/benchmarks.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{filter_indices_eq, with_forced_isa, Isa};

const NEEDLE: u64 = 0;

/// A column of `n` u64 where exactly every `period`-th element equals `NEEDLE`
/// and all others are non-needle. Deterministic (no `rand`/wall-clock):
/// `period = 100` -> ~1% match (sparse), `period = 2` -> ~50% (dense).
fn make_column(n: usize, period: usize) -> Vec<u64> {
    (0..n as u64)
        .map(|i| {
            if (i as usize).is_multiple_of(period) {
                NEEDLE
            } else {
                // Any non-needle value; keep it deterministic and distinct.
                i.wrapping_mul(2654435761) | 1
            }
        })
        .collect()
}

fn bench_one(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    label: &str,
    n: usize,
    values: &[u64],
) {
    let mut out: Vec<u32> = Vec::with_capacity(n);
    group.throughput(Throughput::Elements(values.len() as u64));
    group.bench_with_input(
        BenchmarkId::new(format!("scalar-{label}"), n),
        &n,
        |bn, _| {
            bn.iter(|| {
                out.clear();
                with_forced_isa(Isa::Scalar, || filter_indices_eq(values, NEEDLE, &mut out));
            });
        },
    );
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx512f") {
        group.bench_with_input(
            BenchmarkId::new(format!("avx512-{label}"), n),
            &n,
            |bn, _| {
                bn.iter(|| {
                    out.clear();
                    with_forced_isa(Isa::Avx512, || filter_indices_eq(values, NEEDLE, &mut out));
                });
            },
        );
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        group.bench_with_input(BenchmarkId::new(format!("avx2-{label}"), n), &n, |bn, _| {
            bn.iter(|| {
                out.clear();
                with_forced_isa(Isa::Avx2, || filter_indices_eq(values, NEEDLE, &mut out));
            });
        });
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        group.bench_with_input(BenchmarkId::new(format!("neon-{label}"), n), &n, |bn, _| {
            bn.iter(|| {
                out.clear();
                with_forced_isa(Isa::Neon, || filter_indices_eq(values, NEEDLE, &mut out));
            });
        });
    }
}

fn bench_filter_indices(c: &mut Criterion) {
    let mut group = c.benchmark_group("filter_indices_eq");
    // Include production-scale sizes (65_536 > L2) so a calibration regression
    // that only shows up past L2 can't false-green on the small sizes.
    for &n in &[1024usize, 4096, 16384, 65_536] {
        let sparse = make_column(n, 100); // ~1% match
        let dense = make_column(n, 2); // ~50% match
        bench_one(&mut group, "sparse", n, &sparse);
        bench_one(&mut group, "dense", n, &dense);
    }
    group.finish();
}

criterion_group!(benches, bench_filter_indices);
criterion_main!(benches);
