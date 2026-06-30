//! SPEC-12: `lower_bound` SIMD-over-scalar speedup on L2-resident sorted u64
//! runs (AVX2 / NEON hand-written kernels). Each iteration runs a fixed batch
//! of probes spanning below-min / interior / above-max so branch behaviour and
//! the galloping window are exercised representatively. Throughput is counted
//! in probes. Run on hornbench; record the ratio in BENCHMARKS.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use horndb_simd::{lower_bound, with_forced_isa, Isa};

/// A sorted, strictly-increasing run of `n` u64 (even values), L2-resident at
/// the sizes benched.
fn make_haystack(n: usize) -> Vec<u64> {
    (0..n as u64).map(|x| x * 2).collect()
}

/// 256 deterministic probe values spanning below-min (negative-of-min via 0),
/// interior, and above-max, so each batch mixes hits and the two miss tails.
fn make_probes(n: usize) -> Vec<u64> {
    let max = (n as u64) * 2;
    (0..256u64)
        .map(|i| {
            // Spread across [0, 2*max): the upper half lands above-max (miss),
            // index 0 lands at/below min, the rest interleave interior values.
            i.wrapping_mul(2654435761) % (2 * max.max(1))
        })
        .collect()
}

/// Sum the returned indices across all probes so nothing is optimized away.
fn run_probes(haystack: &[u64], probes: &[u64]) -> usize {
    let mut acc = 0usize;
    for &p in probes {
        acc = acc.wrapping_add(lower_bound(haystack, p));
    }
    acc
}

fn bench_lower_bound(c: &mut Criterion) {
    let mut group = c.benchmark_group("lower_bound");
    for &n in &[1024usize, 4096, 16384] {
        let haystack = make_haystack(n);
        let probes = make_probes(n);
        group.throughput(Throughput::Elements(probes.len() as u64));
        group.bench_with_input(BenchmarkId::new("scalar", n), &n, |bn, _| {
            bn.iter(|| with_forced_isa(Isa::Scalar, || run_probes(&haystack, &probes)));
        });
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx512f") {
            group.bench_with_input(BenchmarkId::new("avx512", n), &n, |bn, _| {
                bn.iter(|| with_forced_isa(Isa::Avx512, || run_probes(&haystack, &probes)));
            });
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            group.bench_with_input(BenchmarkId::new("avx2", n), &n, |bn, _| {
                bn.iter(|| with_forced_isa(Isa::Avx2, || run_probes(&haystack, &probes)));
            });
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            group.bench_with_input(BenchmarkId::new("neon", n), &n, |bn, _| {
                bn.iter(|| with_forced_isa(Isa::Neon, || run_probes(&haystack, &probes)));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_lower_bound);
criterion_main!(benches);
