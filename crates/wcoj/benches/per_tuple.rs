//! SPEC-03 NF1: per-tuple WCOJ overhead. Target **<=5 ns/tuple** on the
//! reference workstation (DuckDB's published baseline is ~2 ns/tuple for simpler
//! operators; SPEC-03 accepts 2.5x for the WCOJ trie machinery). SPEC-03 is the
//! source of truth — an earlier <=2.5 ns internal figure from the SIMD epic
//! (#132) is superseded. Records the `per_tuple` row in docs/benchmarks.md.
//!
//! Two cases, both a 2-variable star join `?x 100 ?y . ?x 101 ?y`. Throughput is
//! set to **output tuples**, so criterion's per-element time *is* ns/tuple.
//!
//! - `two_star_50k` — **descent-bound**: 50k subjects, each yielding only 4
//!   output rows. A full trie descent (two seeks + two `open_level`s + a
//!   depth-1 leapfrog) is paid per 4 tuples, so this measures amortized descent
//!   overhead, not the marginal inner-loop cost. Its floor is well above the
//!   NF1 target and it will not reach it; kept as a regression signal for the
//!   narrow-run descent path.
//! - `wide_4x100k` — **marginal hot path** (the NF1 measurement): 4 subjects,
//!   each with a large object overlap, so one leaf descent drains ~50k tuples
//!   and the SIMD intersect arms. time/tuple is the marginal per-tuple cost NF1
//!   targets. This is the case graded against <=5 ns/tuple.
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

/// High-fan-out marginal-cost source (the NF1 hot path). Few subjects, each
/// with a large object overlap, so one leaf descent drains many tuples and the
/// SIMD intersect arms. time/tuple ≈ the marginal inner-loop cost NF1 targets.
fn build_source_wide(subjects: u64, objects: u64) -> (VecTripleSource, usize) {
    let mut triples = Vec::new();
    for s in 0..subjects {
        for o in 0..objects {
            triples.push(Triple::new(s, 100, o));
            if o % 2 == 0 {
                triples.push(Triple::new(s, 101, o));
            }
        }
    }
    let expected_out = (subjects as usize) * ((objects / 2) as usize);
    (VecTripleSource::from_triples(triples), expected_out)
}

fn bench_per_tuple_wide(c: &mut Criterion) {
    let (source, expected_out) = build_source_wide(4, 100_000);
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
    group.bench_function("wide_4x100k", |b| {
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

criterion_group!(benches, bench_per_tuple, bench_per_tuple_wide);
criterion_main!(benches);
