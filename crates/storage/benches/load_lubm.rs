use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use reasoner_storage::loader::ntriples::load_ntriples_file;
use reasoner_storage::Store;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    if let Ok(p) = std::env::var("LUBM_NT") {
        return PathBuf::from(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/tiny.nt");
    p
}

fn bench_load(c: &mut Criterion) {
    let path = fixture_path();
    let bytes = std::fs::metadata(&path)
        .expect("fixture exists")
        .len();

    // Probe triple count once for throughput annotation.
    let probe = Store::in_memory();
    let stats = load_ntriples_file(&probe, &path).expect("load fixture");
    let triples = stats.triples;

    let mut group = c.benchmark_group("ntriples_load");
    group.throughput(Throughput::Bytes(bytes));
    group.sample_size(10);
    group.bench_function("load_file", |b| {
        b.iter(|| {
            let store = Store::in_memory();
            load_ntriples_file(&store, &path).unwrap();
        });
    });
    eprintln!(
        "fixture: {} triples, {} bytes, last-stats elapsed {} ms (≈{:.2} Mtriples/s)",
        triples,
        bytes,
        stats.elapsed_ms,
        if stats.elapsed_ms == 0 {
            f64::INFINITY
        } else {
            triples as f64 / (stats.elapsed_ms as f64) / 1000.0
        }
    );
    group.finish();
}

criterion_group!(benches, bench_load);
criterion_main!(benches);
