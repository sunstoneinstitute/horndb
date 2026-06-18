//! `rdf:type`-skew bench for SPEC-04 F5 (issue #39).
//!
//! Input: an intersection class `c = c1 ⊓ c2` over a large, skewed `c1` extent
//! (`n` typed subjects), a third of which are also in `c2`. The `cls-int1` rule
//! seeds on the `c1` partition and runs a per-subject membership test — the
//! canonical serial `rdf:type`-scan hot loop. The `Auto` strategy partitions
//! that work across rayon by class id; `Serial` runs the original sequential
//! scan. Both materialise the *identical* closure
//! (`tests/rdf_type_skew_differential.rs`); this bench measures the latency win
//! as the extent grows.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize_with, MaterializeOpts, ParallelStrategy};

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

/// Number of member classes in the intersection. A wider intersection makes
/// each subject's `cls-int1` membership test do more `store.contains` work,
/// which is the per-subject cost F5 parallelises across the skewed extent.
const INT_WIDTH: u64 = 12;

/// `c = c1 ⊓ … ⊓ cW` over a skewed `c1` extent: `n` subjects in `c1`, most of
/// which are also in every other member class, so the per-subject membership
/// test runs the full width before deriving `?x rdf:type c`.
fn skewed_base(v: &Vocabulary, n: u64) -> Vec<Triple> {
    let ty = v.rdf_type.0;
    let c = 9000u64;
    let members: Vec<u64> = (9001..9001 + INT_WIDTH).collect();
    // Build the rdf:List for the intersection members.
    let mut base = Vec::new();
    let list_base = 8000u64;
    base.push(t(c, v.owl_intersection_of.0, list_base));
    for (i, &m) in members.iter().enumerate() {
        let node = list_base + i as u64;
        let next = if i + 1 == members.len() {
            v.rdf_nil.0
        } else {
            list_base + i as u64 + 1
        };
        base.push(t(node, v.rdf_first.0, m));
        base.push(t(node, v.rdf_rest.0, next));
    }
    // n subjects in c1 (the seed extent). 90% are in every member class (so the
    // membership test walks the full width and fires); the rest drop out early.
    for i in 0..n {
        let subj = 1_000_000 + i;
        base.push(t(subj, ty, members[0]));
        if i % 10 != 0 {
            for &m in &members[1..] {
                base.push(t(subj, ty, m));
            }
        }
    }
    base
}

fn run(base: &[Triple], parallel: ParallelStrategy) {
    let v = Vocabulary::synthetic(10_000);
    let mut store = MemStore::new(v);
    store.assert_all(base.iter().copied());
    let mut backend = RuleFiringBackend::new();
    materialize_with(
        &mut store,
        &mut backend,
        MaterializeOpts {
            parallel,
            ..Default::default()
        },
    );
}

fn bench(c: &mut Criterion) {
    let v = Vocabulary::synthetic(10_000);
    let mut group = c.benchmark_group("rdf_type_skew");
    for n in [20_000u64, 50_000, 100_000] {
        let base = skewed_base(&v, n);
        group.bench_with_input(BenchmarkId::new("auto", n), &base, |b, base| {
            b.iter(|| run(base, ParallelStrategy::Auto))
        });
        group.bench_with_input(BenchmarkId::new("serial", n), &base, |b, base| {
            b.iter(|| run(base, ParallelStrategy::Serial))
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
