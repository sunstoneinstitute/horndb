//! SPEC-25 S1: per-tuple MVCC micro-bench. `insert_10k` is the insert-only
//! baseline (must not regress against pre-MVCC numbers); `retract_then_scan_10k`
//! exercises the delete path plus the version-filtered read.
//!
//! Local smoke-check only — do not record these numbers in
//! docs/benchmarks.md. The NF4 write-amplification comparison (stamp columns
//! on copy-on-write vs. delete-bitmap sidecars, CoW vs. in-place append) runs
//! on hornbench under a separate filed follow-up.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn t(i: u64) -> (Term, Term, Term) {
    let n = |s: String| Term::NamedNode(NamedNode::new(s).unwrap());
    (
        n(format!("http://ex/s{i}")),
        n("http://ex/p".to_string()),
        n(format!("http://ex/o{i}")),
    )
}

fn bench(c: &mut Criterion) {
    let rows: Vec<_> = (0..10_000u64).map(t).collect();

    c.bench_function("insert_10k", |b| {
        b.iter(|| {
            let s = Store::in_memory();
            s.insert_triples(&rows).unwrap();
        })
    });

    c.bench_function("retract_then_scan_10k", |b| {
        b.iter(|| {
            let s = Store::in_memory();
            s.insert_triples(&rows).unwrap();
            s.retract_triples(&rows[..1_000]).unwrap();
            let snap = s.snapshot();
            std::hint::black_box(snap.len());
        })
    });
}

/// HDB-122: write latency while a reader is merging the same partition.
///
/// Every insert leaves a new run on the one `http://ex/p` partition, and the
/// reader's next `snapshot().len()` merges them — a sort of the whole
/// partition. While the merge held the `runs` mutex, that merge blocked the
/// next insert, so write latency grew with the *partition*, not the batch.
/// It should now be flat across `SIZES`: read the tail off criterion's
/// reported distribution per size (`target/criterion/.../new/sample.json`
/// carries every sample if you want an exact p99).
///
/// Run on hornbench, not the laptop — a laptop's thermal behaviour under two
/// busy threads is its own variable.
fn write_under_concurrent_reader(c: &mut Criterion) {
    const SIZES: [u64; 3] = [10_000, 100_000, 1_000_000];

    let mut g = c.benchmark_group("write_under_concurrent_reader");
    // A merge of 1M rows dwarfs the single-triple insert being timed, so a
    // handful of samples per size is enough to see the tail.
    g.sample_size(30);
    for n in SIZES {
        let store = Arc::new(Store::in_memory());
        let rows: Vec<_> = (0..n).map(t).collect();
        store.insert_triples(&rows).unwrap();
        drop(rows);

        let stop = Arc::new(AtomicBool::new(false));
        let (reader_store, reader_stop) = (store.clone(), stop.clone());
        let reader = std::thread::spawn(move || {
            while !reader_stop.load(Ordering::Acquire) {
                std::hint::black_box(reader_store.snapshot().len());
            }
        });

        let mut next = n;
        g.bench_function(BenchmarkId::from_parameter(n), |b| {
            b.iter(|| {
                next += 1;
                store.insert_triples(&[t(next)]).unwrap();
            })
        });

        stop.store(true, Ordering::Release);
        reader.join().unwrap();
    }
    g.finish();
}

criterion_group!(benches, bench, write_under_concurrent_reader);
criterion_main!(benches);
