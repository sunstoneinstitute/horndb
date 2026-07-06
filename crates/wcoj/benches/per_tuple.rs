//! SPEC-12 acceptance #2 / SPEC-03 NF1: per-tuple WCOJ overhead.
//! Target: <=2.5 ns/tuple on hornbench (from the <=5 ns Stage-1 envelope toward
//! DuckDB's ~2 ns). Records the `per_tuple` row in docs/benchmarks.md.
//!
//! The bench runs a 2-variable star join whose output is large and
//! seek-dominated, so the measured time/tuple isolates the cursor seek +
//! leapfrog inner loop. Throughput is set to **output tuples** so criterion's
//! per-element time *is* ns/tuple.
//!
//! Run bench numbers on the `hornbench` host only (see CLAUDE.md); local runs
//! are smoke-checks, not recordings.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::vec_source::VecTripleSource;

/// A two-star join: `?x 100 ?y . ?x 101 ?y` — output is the intersection of the
/// two predicates' (s, o) pairs at the inner `?y` variable, which stresses the
/// same-level seek/intersect inner loop. Predicate 100 emits `?y` in `0..8`;
/// predicate 101 emits only even `?y`, so each `?x` yields 4 output rows.
fn build_source(n: u64) -> (VecTripleSource, usize) {
    let mut triples = Vec::new();
    for s in 0..n {
        for o in 0..8u64 {
            triples.push(Triple::new(s, 100, o));
            if o % 2 == 0 {
                triples.push(Triple::new(s, 101, o));
            }
        }
    }
    let expected_out = (n as usize) * 4; // o in {0, 2, 4, 6}
    (VecTripleSource::from_triples(triples), expected_out)
}

fn bench_per_tuple(c: &mut Criterion) {
    let n = 50_000u64;
    let (source, expected_out) = build_source(n);

    // ?x 100 ?y . ?x 101 ?y
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(100), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(101), Term::Var(Var(1))),
    ]);
    let plan = ExecutionPlan {
        kind: PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1)],
    };

    let mut group = c.benchmark_group("per_tuple");
    group.throughput(Throughput::Elements(expected_out as u64));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("two_star_50k", |b| {
        b.iter(|| {
            let exec = WcojExecutor::new(&source, &bgp, &plan, CancelToken::new());
            let mut count = 0usize;
            for batch in exec.into_iter() {
                count += batch.unwrap().num_rows();
            }
            assert_eq!(count, expected_out);
            std::hint::black_box(count)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_per_tuple);
criterion_main!(benches);
