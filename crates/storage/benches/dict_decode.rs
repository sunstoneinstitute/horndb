//! SPEC-12 acceptance #4 / NF4: bulk inline-int decode ≥4× scalar.
//! Run on hornbench; record the ratio in docs/benchmarks.md.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use horndb_simd::{with_forced_isa, Isa};
use horndb_storage::dictionary::Dictionary;
use horndb_storage::term::TermId;

fn make_ids(n: usize) -> Vec<TermId> {
    (0..n as i32).map(TermId::inline_int).collect()
}

fn bench_decode(c: &mut Criterion) {
    let ids = make_ids(1 << 16); // 64Ki ids, L2-ish
    let mut group = c.benchmark_group("dict_decode_inline_int");
    group.throughput(Throughput::Elements(ids.len() as u64));
    group.bench_function("scalar", |b| {
        b.iter(|| with_forced_isa(Isa::Scalar, || Dictionary::decode_inline_ints(&ids)));
    });
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        group.bench_function("avx2", |b| {
            b.iter(|| with_forced_isa(Isa::Avx2, || Dictionary::decode_inline_ints(&ids)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
