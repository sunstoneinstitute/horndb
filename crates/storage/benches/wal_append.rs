//! SPEC-25 S3 write-ahead log: the cost of one logged batch under each fsync
//! policy, and replay throughput on reopen.
//!
//! `HORNDB_WAL_BATCH` quads per batch (default 10,000), fresh terms each
//! batch so every record carries its dictionary appends. `append/every_batch`
//! is the default policy (one fsync per record); `append/timed` fsyncs at
//! most once a second, so it measures the record encode + write alone.
//! `replay` reopens a log of `HORNDB_WAL_REPLAY_BATCHES` (default 50) such
//! batches. Record on hornbench only (`scripts/bench/audit-pass.sh`, leg `wal`).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use horndb_storage::{GraphId, Store, SyncPolicy, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};
use std::time::Duration;

fn batch(seq: u64, n: usize) -> Vec<(GraphId, Term, Term, Term)> {
    let p = Term::NamedNode(NamedNode::new("http://ex/p").unwrap());
    (0..n)
        .map(|i| {
            let s = Term::NamedNode(NamedNode::new(format!("http://ex/s{seq}/{i}")).unwrap());
            let o =
                Term::NamedNode(NamedNode::new(format!("http://ex/o{seq}/{}", i % 97)).unwrap());
            (DEFAULT_GRAPH, s, p.clone(), o)
        })
        .collect()
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn bench(c: &mut Criterion) {
    let n = env("HORNDB_WAL_BATCH", 10_000);
    let replay_batches = env("HORNDB_WAL_REPLAY_BATCHES", 50);

    let mut g = c.benchmark_group("append");
    g.throughput(Throughput::Elements(n as u64));
    for (name, policy) in [
        ("every_batch", SyncPolicy::EveryBatch),
        ("timed", SyncPolicy::Every(Duration::from_secs(1))),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with(dir.path(), policy).unwrap();
        let mut seq = 0u64;
        g.bench_function(name, |b| {
            b.iter_batched(
                || {
                    seq += 1;
                    batch(seq, n)
                },
                |quads| store.insert_quads(&quads).unwrap(),
                BatchSize::LargeInput,
            )
        });
    }
    g.finish();

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for seq in 0..replay_batches as u64 {
        store.insert_quads(&batch(seq, n)).unwrap();
    }
    drop(store);
    let mut g = c.benchmark_group("replay");
    g.throughput(Throughput::Elements((n * replay_batches) as u64));
    g.sample_size(10);
    g.bench_function("open", |b| b.iter(|| Store::open(dir.path()).unwrap()));
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
