//! SPEC-12 acceptance #3 / NF2: intersect SIMD-over-scalar speedup on
//! L2-resident sorted u64 runs. Target: >=4x on AVX-512, >=2x on NEON.
//! Run on hornbench; record the ratio in BENCHMARKS.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{intersect, with_forced_isa, Isa};

/// Two sorted, deduped runs of `n` u64 with ~50% overlap, L2-resident at
/// n = 4096 (32 KiB each, fits a 512 KiB L2 with room).
fn make_runs(n: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..n as u64).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..n as u64).map(|x| x * 2 + (x % 2)).collect();
    (a, b)
}

fn bench_intersect(c: &mut Criterion) {
    let mut group = c.benchmark_group("intersect");
    for &n in &[1024usize, 4096, 16384] {
        let (a, b) = make_runs(n);
        group.throughput(Throughput::Elements((a.len() + b.len()) as u64));
        group.bench_with_input(BenchmarkId::new("scalar", n), &n, |bn, _| {
            bn.iter(|| {
                let mut out = Vec::with_capacity(n);
                with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut out));
                out
            });
        });
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f") {
            group.bench_with_input(BenchmarkId::new("avx512", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Avx512, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            group.bench_with_input(BenchmarkId::new("avx2", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Avx2, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            group.bench_with_input(BenchmarkId::new("neon", n), &n, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(n);
                    with_forced_isa(Isa::Neon, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_intersect);
criterion_main!(benches);
