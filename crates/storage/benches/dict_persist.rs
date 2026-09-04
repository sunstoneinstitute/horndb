//! SPEC-25 S2 persistent dictionary: flush, reopen, and the two probe
//! directions against the mapped base.
//!
//! Synthetic LUBM-shaped IRIs, `HORNDB_DICT_PERSIST_TERMS` distinct terms
//! (default 1,000,000). Probes run in document order over the whole set, so
//! `term_to_id` / `id_to_term` report the cost of one probe that misses the
//! overlay and hits the base — the reopen case, with no repeat cache in front.
//! Record on hornbench only (`scripts/bench/audit-pass.sh`, leg `dict_persist`).

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use horndb_storage::{Dictionary, TermId};
use oxrdf::{NamedNode, Term};

const PROBES_PER_ITER: usize = 10_000;

fn lubm_terms(n: usize) -> Vec<Term> {
    (0..n)
        .map(|i| {
            let (uni, dept, student) = (i / 100_000, (i / 5_000) % 20, i % 5_000);
            Term::NamedNode(
                NamedNode::new(format!(
                    "http://www.Department{dept}.University{uni}.edu/UndergraduateStudent{student}"
                ))
                .unwrap(),
            )
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let n: usize = std::env::var("HORNDB_DICT_PERSIST_TERMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let terms = lubm_terms(n);
    let dict = Dictionary::new();
    let ids: Vec<TermId> = terms.iter().map(|t| dict.intern(t).unwrap()).collect();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dict.base");

    let mut g = c.benchmark_group("dict_persist");
    g.sample_size(10);
    g.throughput(Throughput::Elements(n as u64));
    g.bench_function("flush", |b| b.iter(|| dict.flush(&path).unwrap()));
    g.bench_function("open", |b| b.iter(|| Dictionary::open(&path).unwrap()));
    g.finish();

    let reopened = Dictionary::open(&path).unwrap();
    assert_eq!(reopened.len(), n);
    let mut g = c.benchmark_group("dict_persist_probe");
    g.throughput(Throughput::Elements(PROBES_PER_ITER as u64));
    let mut at = 0usize;
    g.bench_function("term_to_id", |b| {
        b.iter(|| {
            for _ in 0..PROBES_PER_ITER {
                let i = at % n;
                at += 1;
                assert_eq!(reopened.get(&terms[i]), Some(ids[i]));
            }
        })
    });
    at = 0;
    g.bench_function("id_to_term", |b| {
        b.iter(|| {
            for _ in 0..PROBES_PER_ITER {
                let i = at % n;
                at += 1;
                assert!(reopened.lookup(ids[i]).is_some());
            }
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
