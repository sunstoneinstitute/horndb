//! Bench: SPEC-05 acceptance criterion 3.
//!
//! "owl:sameAs equivalence classes on a synthetic graph of 10M sameAs
//!  assertions across 1M canonical entities: union-find construction ≤5 s;
//!  canonical-representative lookup ≤100 ns average."

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use horndb_closure::sameas::EquivClasses;
use horndb_closure::types::DictId;

fn synth_pairs(n_assertions: usize, n_canonical: u64, seed: u64) -> Vec<(DictId, DictId)> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut pairs = Vec::with_capacity(n_assertions);
    // Each assertion: pick two ids in [0, 10*n_canonical), random.
    let range = 10 * n_canonical;
    for _ in 0..n_assertions {
        let a = rng.random_range(0..range);
        let b = rng.random_range(0..range);
        pairs.push((DictId(a), DictId(b)));
    }
    pairs
}

fn bench_sameas_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("sameas_construction");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(10);

    for &(n_assert, n_canon) in &[
        (100_000usize, 10_000u64),
        (1_000_000, 100_000),
        (10_000_000, 1_000_000), // SPEC-05 acceptance criterion 3
    ] {
        let pairs = synth_pairs(n_assert, n_canon, 0xC0FFEE);
        group.throughput(Throughput::Elements(n_assert as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{n_assert}@{n_canon}")),
            &pairs,
            |b, pairs| {
                b.iter(|| {
                    let mut ec = EquivClasses::with_capacity(n_canon as usize * 11);
                    for &(a, b) in pairs {
                        ec.union(a, b);
                    }
                    ec
                });
            },
        );
    }
    group.finish();
}

fn bench_canonical_lookup(c: &mut Criterion) {
    let pairs = synth_pairs(1_000_000, 100_000, 0xBEEF);
    let mut ec = EquivClasses::with_capacity(2_000_000);
    for &(a, b) in &pairs {
        ec.union(a, b);
    }
    // After construction, all parent pointers eventually compress.
    // Warm by walking once.
    for &(a, _) in pairs.iter().take(1000) {
        let _ = ec.canonical(a);
    }
    let probes: Vec<DictId> = pairs.iter().take(10_000).map(|p| p.0).collect();

    let mut group = c.benchmark_group("sameas_lookup");
    group.throughput(Throughput::Elements(probes.len() as u64));
    group.bench_function("canonical_x10k", |b| {
        b.iter(|| {
            let mut sum: u64 = 0;
            for id in &probes {
                if let Some(c) = ec.canonical(*id) {
                    sum = sum.wrapping_add(c.0);
                }
            }
            sum
        });
    });
    group.finish();
}

criterion_group!(benches, bench_sameas_construction, bench_canonical_lookup);
criterion_main!(benches);
