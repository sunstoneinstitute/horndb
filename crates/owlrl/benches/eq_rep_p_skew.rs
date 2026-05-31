//! Adversarial skew bench for `eq-rep-p` (TASKS.md #2 / SPEC-04 F5).
//!
//! Input: `k` predicates all in one `owl:sameAs` class, each carrying `rows`
//! triples. The naïve path regenerates `O(k·rows)` candidate triples per
//! predicate per round (`O(k²·rows)` total, mostly deduplicated); the
//! optimized path unions once. The materialised output is identical for both
//! — this bench measures the *work* difference, demonstrating the optimized
//! path's cost stays bounded as `k` grows.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize_with, EqRepPStrategy, MaterializeOpts};

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

/// Build `k` mutually-sameAs predicates, each with `rows` distinct triples.
fn adversarial_base(v: &Vocabulary, k: u64, rows: u64) -> Vec<Triple> {
    let same = v.owl_same_as.0;
    let preds: Vec<u64> = (1_000..1_000 + k).collect();
    let mut base = Vec::new();
    for w in preds.windows(2) {
        base.push(t(w[0], same, w[1]));
    }
    for (i, &p) in preds.iter().enumerate() {
        for r in 0..rows {
            base.push(t(10_000 + i as u64 * rows + r, p, 50_000 + r));
        }
    }
    base
}

fn run(base: &[Triple], strat: EqRepPStrategy) {
    let v = Vocabulary::synthetic(10_000);
    let mut store = MemStore::new(v);
    store.assert_all(base.iter().copied());
    let mut backend = RuleFiringBackend::new();
    materialize_with(
        &mut store,
        &mut backend,
        MaterializeOpts { eq_rep_p: strat },
    );
}

fn bench(c: &mut Criterion) {
    let v = Vocabulary::synthetic(10_000);
    let rows = 8u64;
    let mut group = c.benchmark_group("eq_rep_p_skew");
    for k in [8u64, 16, 32] {
        let base = adversarial_base(&v, k, rows);
        group.bench_with_input(BenchmarkId::new("optimized", k), &base, |b, base| {
            b.iter(|| run(base, EqRepPStrategy::Optimized))
        });
        group.bench_with_input(BenchmarkId::new("naive", k), &base, |b, base| {
            b.iter(|| run(base, EqRepPStrategy::Naive))
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
