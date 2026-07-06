//! SPEC-12: `gather` (indexed load) SIMD-over-scalar measurement on
//! L2-resident u64 bases (AVX2 `vpgatherqq` hand-written kernel).
//!
//! HYPOTHESIS THIS BENCH EXISTS TO TEST: AVX2 64-bit gather (`vpgatherqq`) is
//! frequently NO faster — and on several microarchitectures *slower* — than a
//! scalar indexed-load loop, because the hardware gather is internally
//! micro-sequenced. This bench compares the forced AVX2 path against scalar so
//! the maintainer can confirm whether the AVX2 kernel is a pessimization on the
//! hornbench host and decide whether to keep dispatching to it. Throughput is
//! counted in gathered indices. Run on hornbench; record in docs/benchmarks.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{gather, with_forced_isa, Isa};

/// A base run of `n` u64 and a deterministic permutation-ish index array of
/// length `n` with pseudo-random in-bounds indices (Knuth multiplicative hash,
/// no `rand`/wall-clock — fully reproducible across hosts and runs).
fn make_inputs(n: usize) -> (Vec<u64>, Vec<u32>) {
    let base: Vec<u64> = (0..n as u64).map(|x| x.wrapping_mul(0x9E37_79B9)).collect();
    let indices: Vec<u32> = (0..n)
        .map(|i| (i.wrapping_mul(2654435761) % n) as u32)
        .collect();
    (base, indices)
}

fn bench_gather(c: &mut Criterion) {
    let mut group = c.benchmark_group("gather");
    // Include production-scale sizes (65_536, 262_144 = 2 MB > L2) so the scalar
    // win on a > L2 base is visible and a calibration regression can't
    // false-green on the small L2-resident sizes.
    for &n in &[1024usize, 4096, 16384, 65_536, 262_144] {
        let (base, indices) = make_inputs(n);
        group.throughput(Throughput::Elements(indices.len() as u64));
        let mut out = Vec::with_capacity(n);
        group.bench_with_input(BenchmarkId::new("scalar", n), &n, |bn, _| {
            bn.iter(|| {
                out.clear();
                with_forced_isa(Isa::Scalar, || gather(&base, &indices, &mut out));
            });
        });
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f") {
            group.bench_with_input(BenchmarkId::new("avx512", n), &n, |bn, _| {
                bn.iter(|| {
                    out.clear();
                    with_forced_isa(Isa::Avx512, || gather(&base, &indices, &mut out));
                });
            });
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            group.bench_with_input(BenchmarkId::new("avx2", n), &n, |bn, _| {
                bn.iter(|| {
                    out.clear();
                    with_forced_isa(Isa::Avx2, || gather(&base, &indices, &mut out));
                });
            });
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            group.bench_with_input(BenchmarkId::new("neon", n), &n, |bn, _| {
                bn.iter(|| {
                    out.clear();
                    with_forced_isa(Isa::Neon, || gather(&base, &indices, &mut out));
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_gather);
criterion_main!(benches);
