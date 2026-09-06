//! Insert-throughput micro-benchmark.
//!
//! Stage 1 purpose: provide a `cargo bench` entry point so regressions
//! show up in CI. NF1/NF2 numbers are Stage 2 deliverables and will
//! need an LUBM-shaped fixture; here we use a synthetic chain of P
//! edges and assert nothing about wall time — criterion just records
//! the number for later comparison.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_incremental::{Circuit, HashJoinRule, NaryPlan};
use horndb_wcoj::{Term, TriplePattern, Var};

const P: u64 = 7;

fn transitive_p() -> HashJoinRule {
    let x = Term::Var(Var(0));
    let y = Term::Var(Var(1));
    let z = Term::Var(Var(2));
    let p = Term::Bound(P);
    HashJoinRule::new(
        1,
        TriplePattern::new(x, p, y),
        TriplePattern::new(y, p, z),
        TriplePattern::new(x, p, z),
    )
    .expect("transitive-on-P is a valid HashJoinRule shape")
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    // HDB-174 measurement (2026-09-06, this laptop, debug-instrumented probe,
    // not release/hornbench): a chain's transitive closure is a complete
    // order over its N+1 nodes, so `Circuit::tick`'s doubling fixpoint joins
    // extents that converge to ~N²/2 rows against each other. The total
    // number of (i, mid, j) compositions the bilinear self-join must sum is
    // Θ(N³) regardless of join algorithm (hash join only removes
    // non-matching candidates; on a converged chain almost every candidate
    // matches). Measured: N=1,000 takes ~35 s; N=10,000 is projected at
    // ~1,000× that (hours), not "seconds". Flagged to the task's requester —
    // see the HDB-174 completion report.
    for &n in &[100u64, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut circuit = Circuit::new();
                let mut plan = NaryPlan::new();
                plan.push_join(Box::new(transitive_p()));
                circuit.add_plan(plan, 1);
                for i in 0..n {
                    circuit.assert_triple((i, P, i + 1));
                }
                circuit.tick();
                std::hint::black_box(circuit.derived_base().len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_insert);
criterion_main!(benches);
