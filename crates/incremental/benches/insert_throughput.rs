//! Insert-throughput micro-benchmark.
//!
//! Stage 1 purpose: provide a `cargo bench` entry point so regressions
//! show up in CI. NF1/NF2 numbers are Stage 2 deliverables and will
//! need an LUBM-shaped fixture; here we use a synthetic chain of P
//! edges and assert nothing about wall time — criterion just records
//! the number for later comparison.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_incremental::{BilinearRule, Circuit, NaryPlan, RuleId, TripleId, Zset};

const P: u64 = 7;

struct TransitiveP;
impl BilinearRule for TransitiveP {
    fn id(&self) -> RuleId {
        1
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys {
                    out.add((*xs, P, *yo), ma * mb);
                }
            }
        }
        out
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    // Stage-1 smoke sizes only: the reference TransitiveP join is a
    // naïve O(n²) nested loop and the fixed-point in Circuit::tick
    // recomputes the transitive closure every round. A 1000-edge
    // chain would compile to ~500K closure edges times MAX_ROUNDS;
    // measurable but Stage-2-territory once SPEC-04 emits hash joins.
    for &n in &[10u64, 50, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut circuit = Circuit::new();
                let mut plan = NaryPlan::new();
                plan.push_join(Box::new(TransitiveP));
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
