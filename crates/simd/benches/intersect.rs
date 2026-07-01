//! SPEC-12 acceptance #3 / NF2: intersect SIMD-over-scalar speedup on
//! L2-resident sorted u64 runs. Target: >=4x on AVX-512, >=2x on NEON.
//! Run on hornbench; record the ratio in BENCHMARKS.md.
//!
//! Shapes cover BOTH the balanced regime (where block-vs-block SIMD wins) AND
//! the skewed regime (where leapfrog feeds `intersect` lopsided `active_run`s
//! and the production gate switches to galloping). The original balanced-only
//! bench false-greened the −7% SPB regression bisected to `ccecd5f`; keep a
//! skewed shape here so that blind spot stays closed. The `auto` arm measures
//! the *unforced* production path (the size-ratio gate), while the forced arms
//! isolate one kernel.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{intersect, with_forced_isa, Isa};

/// Two sorted, deduped runs of lengths `na`/`nb` with partial overlap. Balanced
/// (`na == nb`) is L2-resident at 4096 (32 KiB each); the skewed shapes put a
/// small side against a large one to exercise the galloping path.
fn make_runs(na: usize, nb: usize) -> (Vec<u64>, Vec<u64>) {
    let a: Vec<u64> = (0..na as u64).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..nb as u64).map(|x| x * 2 + (x % 2)).collect();
    (a, b)
}

fn bench_intersect(c: &mut Criterion) {
    let mut group = c.benchmark_group("intersect");
    // (na, nb): balanced, then increasingly skewed (leapfrog-realistic).
    let shapes: &[(usize, usize)] = &[
        (4096, 4096),
        (16384, 16384),
        (64, 1_000_000),
        (256, 1_000_000),
    ];
    for &(na, nb) in shapes {
        let (a, b) = make_runs(na, nb);
        let label = format!("{na}x{nb}");
        group.throughput(Throughput::Elements((a.len() + b.len()) as u64));

        // Production gate (galloping for skew, block-SIMD for balanced).
        group.bench_with_input(BenchmarkId::new("auto", &label), &label, |bn, _| {
            bn.iter(|| {
                let mut out = Vec::with_capacity(na.min(nb));
                intersect(&a, &b, &mut out);
                out
            });
        });
        group.bench_with_input(BenchmarkId::new("scalar", &label), &label, |bn, _| {
            bn.iter(|| {
                let mut out = Vec::with_capacity(na.min(nb));
                with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut out));
                out
            });
        });
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f") {
            group.bench_with_input(BenchmarkId::new("avx512", &label), &label, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(na.min(nb));
                    with_forced_isa(Isa::Avx512, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            group.bench_with_input(BenchmarkId::new("avx2", &label), &label, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(na.min(nb));
                    with_forced_isa(Isa::Avx2, || intersect(&a, &b, &mut out));
                    out
                });
            });
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            group.bench_with_input(BenchmarkId::new("neon", &label), &label, |bn, _| {
                bn.iter(|| {
                    let mut out = Vec::with_capacity(na.min(nb));
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
