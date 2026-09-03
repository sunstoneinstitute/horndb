//! HDB-121: cost of the dictionary sweep under append + retract churn.
//!
//! `churn_4x1k_no_gc` is the baseline — four rounds of "insert 1k fresh
//! triples, retract them" — and `churn_4x1k_compact_gc` is the same work plus
//! the `compact()` after each round that reclaims the rows and sweeps the
//! terms. The gap is what a maintenance compaction costs per churn round; the footprint side (live terms and
//! forward-map keys returning to a constant) is asserted in
//! `tests/dictionary_gc.rs`, not here — criterion measures time, not RSS.
//!
//! Local smoke-check only — do not record these numbers in
//! docs/benchmarks.md. The recorded run belongs on hornbench.

use criterion::{criterion_group, criterion_main, Criterion};
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

const ROUND: usize = 1_000;

fn rows(round: usize) -> Vec<(Term, Term, Term)> {
    let n = |s: String| Term::NamedNode(NamedNode::new(s).unwrap());
    (0..ROUND)
        .map(|i| {
            (
                n(format!("http://ex/s{round}-{i}")),
                n("http://ex/p".to_string()),
                n(format!("http://ex/o{round}-{i}")),
            )
        })
        .collect()
}

fn churn(store: &Store, round: usize) {
    let batch = rows(round);
    store.insert_triples(&batch).unwrap();
    store.retract_triples(&batch).unwrap();
}

fn bench(c: &mut Criterion) {
    // A fresh store per iteration, so the dead rows one iteration leaves
    // behind do not make the next one slower (the no-GC arm accumulates
    // them by construction, which is the point being measured).
    c.bench_function("churn_4x1k_no_gc", |b| {
        b.iter(|| {
            let store = Store::in_memory();
            for round in 0..4 {
                churn(&store, round);
            }
        })
    });

    c.bench_function("churn_4x1k_compact_gc", |b| {
        b.iter(|| {
            let store = Store::in_memory();
            for round in 0..4 {
                churn(&store, round);
                store.compact();
            }
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
