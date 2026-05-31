//! SPEC-03 acceptance criterion #2: on the 4-cycle query
//!   (?a -p-> ?b -p-> ?c -p-> ?d -p-> ?a)
//! over a synthetic graph of ~10^6 edges, WCOJ outperforms binary-hash join
//! by ≥10× — the canonical worst-case-optimal-join win case.
//!
//! The win case is a *skewed* graph, not a uniform random one. A worst-case-
//! optimal join only dominates a binary join when the binary join is forced
//! to materialise an intermediate result far larger than the final output.
//! On a uniform low-degree graph (the shape used before #1) the 4-cycle has
//! no such blow-up, so WCOJ and binary-hash run within ~1× of each other.
//!
//! [`SyntheticGraph::skewed_four_cycle`] builds the canonical win case: four
//! vertex layers A→B→C→D on a single predicate, with high-out-degree hubs in
//! C and a thin, dedicated D→A closure. A binary-hash join materialises the
//! full 3-path relation `(a,b,c,d)` — size `#2-paths · hub_out` — over *every*
//! source before it can apply the closure. WCOJ binds `[a,b,c,d]` one variable
//! at a time, depth-first, and never materialises an intermediate: for almost
//! every `(a,b,c)` prefix the cycle-closing intersection `out(c) ∩ in(a)` at
//! the last variable is empty, so it backtracks in O(1) without expanding a
//! hub's `hub_out` out-edges — its cost tracks the number of 2-paths, a
//! ≈`hub_out` advantage. See the generator docs and `tests/skewed_four_cycle.rs`
//! (which pins both executors against an independent brute-force 4-cycle count,
//! including the rotational matches a single-predicate cycle admits) for the
//! full rationale.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::synthetic::{SkewedFourCycle, SyntheticGraph};
use horndb_wcoj::source::vec_source::VecTripleSource;

/// Canonical WCOJ-win 4-cycle graph: ~10^6 edges, `hub_out = 32` blow-up.
/// `#2-paths = sources·a_out·hubs = 10^6`; the binary-hash join materialises
/// `#2-paths · hub_out ≈ 3.2·10^7` 3-paths, while the 4-cycle output is only
/// `close_sources·a_out·close_sinks`-bounded (a few dozen rows).
const GATE_PARAMS: SkewedFourCycle = SkewedFourCycle {
    sources: 10_000,
    a_out: 2,
    hubs: 50,
    hub_out: 32,
    bulk_sinks: 10_000,
    close_sources: 4,
    close_sinks: 2,
    predicate: 10,
    seed: 0xDEAD_BEEF,
};

fn make_4_cycle_bgp() -> Bgp {
    let p = GATE_PARAMS.predicate;
    Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ])
}

fn bench_four_cycle(c: &mut Criterion) {
    let edges = SyntheticGraph::skewed_four_cycle_edges(&GATE_PARAMS);
    let n_edges = edges.len();
    let source = VecTripleSource::from_triples(edges);
    let bgp = make_4_cycle_bgp();

    // One-time shape report (stdout; criterion does not capture this).
    eprintln!(
        "four_cycle (skewed win case): {n_edges} edges, hub_out={}",
        GATE_PARAMS.hub_out
    );

    let mut group = c.benchmark_group("four_cycle");
    // The binary-hash leg materialises ~3.2·10^7 intermediate rows and takes
    // several seconds per iteration; 10 samples keeps the run bounded.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("wcoj", |b| {
        b.iter(|| {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: vec![Var(0), Var(1), Var(2), Var(3)],
            };
            let exec = WcojExecutor::new(&source, &bgp, &plan, CancelToken::new());
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.bench_function("binary_hash", |b| {
        b.iter(|| {
            let exec = BinaryHashExecutor::new(
                &source,
                &bgp,
                vec![Var(0), Var(1), Var(2), Var(3)],
                CancelToken::new(),
            );
            let mut rows = 0u64;
            for batch in exec.into_iter() {
                rows += batch.unwrap().num_rows() as u64;
            }
            criterion::black_box(rows);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_four_cycle);
criterion_main!(benches);
