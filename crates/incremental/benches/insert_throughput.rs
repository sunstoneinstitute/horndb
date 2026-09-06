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
    // ponytail: the sweep stays small because this fixture is cubic by
    // *shape*, not by join algorithm. A chain's transitive closure is a
    // complete order over its N+1 nodes, so the closing rounds self-join
    // two ~N**2/2-row extents and the bilinear decomposition must sum
    // C(N+1, 3) ~ N**3/6 compositions. Hash-joining (HDB-174) only drops
    // non-matching candidates, and on a converged chain almost every
    // candidate matches: measured N=1,000 is ~35 s per iteration, so
    // N=10,000 is hours. Growing this sweep needs a sparser fixture whose
    // closure does not saturate (HDB-182), not a faster kernel.
    for &n in &[10u64, 50, 100] {
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
