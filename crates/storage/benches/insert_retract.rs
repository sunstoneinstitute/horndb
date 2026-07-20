//! SPEC-25 S1: per-tuple MVCC micro-bench. `insert_10k` is the insert-only
//! baseline (must not regress against pre-MVCC numbers); `retract_then_scan_10k`
//! exercises the delete path plus the version-filtered read.
//!
//! Local smoke-check only — do not record these numbers in
//! docs/benchmarks.md. The NF4 write-amplification comparison (stamp columns
//! on copy-on-write vs. delete-bitmap sidecars, CoW vs. in-place append) runs
//! on hornbench under a separate filed follow-up.

use criterion::{criterion_group, criterion_main, Criterion};
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

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

criterion_group!(benches, bench);
criterion_main!(benches);
