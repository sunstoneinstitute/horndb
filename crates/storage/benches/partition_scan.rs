//! SPEC-12 acceptance #4 / SPEC-02 NF2: rdf:type partition scan reaches
//! ≥80% STREAM-Triad bandwidth. Run NUMA-pinned on hornbench; record GB/s
//! and the STREAM-Triad fraction in docs/benchmarks.md.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use horndb_storage::partition::PartitionBuilder;
use horndb_storage::partition::PredicatePartition;
use horndb_storage::term::TermId;

/// A large rdf:type-shaped partition: many subjects, a modest set of class
/// objects, so `subjects_with_object(class)` scans the full object column.
fn build_partition(n: u64) -> PredicatePartition {
    let mut b = PartitionBuilder::default();
    for s in 0..n {
        b.append(TermId(s), TermId(s % 1000)); // 1000 classes
    }
    b.build()
}

fn bench_scan(c: &mut Criterion) {
    let n = 10_000_000u64; // 10M rows; object column = 80 MB, RAM-resident
    let part = build_partition(n);
    let bytes = n * std::mem::size_of::<u64>() as u64; // object column bytes moved
    let mut group = c.benchmark_group("rdf_type_partition_scan");
    group.throughput(Throughput::Bytes(bytes));
    group.bench_function("subjects_with_object", |b| {
        b.iter(|| std::hint::black_box(part.subjects_with_object(500)));
    });
    group.finish();
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
