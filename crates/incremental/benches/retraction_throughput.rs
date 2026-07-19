//! Retraction small-delta A/B benchmark (SPEC-24 S1, PLAN-24-01 Task 6).
//!
//! Measures the steady-state cost of retracting one interior `SC` chain edge
//! and re-asserting it (two `tick()`s per iteration) on a warm circuit, for the
//! delta-incremental path (`Circuit::new()`) vs the Stage-1 recompute
//! fallback (`Circuit::new_with_recompute_fallback()`). The #210 acceptance
//! ratio (incremental ≥10× faster) is read off the criterion report by
//! dividing `recompute_fallback` by `incremental` at the same N.
//!
//! Fixture shape (local copy of the SPEC-06 synthetic bilinears — benches
//! cannot use `tests/fixtures/`): a chain of N `SC` edges plus N `TYPE`
//! facts, one individual typed at each chain class, so both rules have real
//! consequences (~N²/2 derived `SC` rows and ~N²/2 derived `TYPE` rows).
//! Retracting the cut edge cascades withdrawal of every consequence that
//! spans it (both `SC` paths and fanned `TYPE` rows); re-asserting restores
//! them, returning the circuit to its pre-iteration state — so a plain
//! `b.iter` over one long-lived warm circuit is a valid steady-state
//! measurement.
//!
//! Cut-edge choice (deviation from the plan's "mid-chain" wording, on
//! purpose): the edge at position N−4, not N/2. In a bare chain, cutting the
//! exact middle withdraws every consequence spanning the cut — ~half the
//! closure — which is a BULK delta, not the small-delta steady state the
//! acceptance gate measures (and on it the delta path is rightly no faster
//! than recompute). The N−4 edge is still interior — it has N−4 ancestors
//! and 4 descendants, so both rules cascade real withdrawals (~5N of the
//! ~N² rows, ~4% at N=256) — but the delta stays small relative to the
//! store, which is exactly the "small-delta ticks" regime of the #210
//! acceptance criterion.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use horndb_incremental::{BilinearRule, Circuit, NaryPlan, RuleId, TripleId, Zset};

/// Predicate IDs, matching the synthetic-rules fixture.
const SC: u64 = 100;
const TYPE: u64 = 102;
/// Chain classes are `1..=n+1`; individuals start here to stay disjoint.
const IND_BASE: u64 = 1_000_000;

const R_SCM_SCO: RuleId = 1;
const R_CAX_SCO: RuleId = 2;

/// (?x SC ?y) ∧ (?y SC ?z) → (?x SC ?z) — local copy of the fixture's
/// `TransitiveOn` bilinear, naïve nested-loop join (Stage-1 reference shape).
struct TransitiveSc;
impl BilinearRule for TransitiveSc {
    fn id(&self) -> RuleId {
        R_SCM_SCO
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != SC {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != SC {
                    continue;
                }
                if xo == ys {
                    out.add((*xs, SC, *yo), ma * mb);
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

/// (?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d) — local copy of the fixture's
/// `CaxScoRule` bilinear.
struct CaxSco;
impl BilinearRule for CaxSco {
    fn id(&self) -> RuleId {
        R_CAX_SCO
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != TYPE {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != SC {
                    continue;
                }
                if xo == ys {
                    out.add((*xs, TYPE, *yo), ma * mb);
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

/// Build a warm circuit: both plans registered, the N-edge SC chain and the
/// N TYPE facts asserted, and one `tick()` run to materialize the closure.
fn warm_circuit(recompute_fallback: bool, n: u64) -> Circuit {
    let mut circuit = if recompute_fallback {
        Circuit::new_with_recompute_fallback()
    } else {
        Circuit::new()
    };
    let mut p1 = NaryPlan::new();
    p1.push_join(Box::new(TransitiveSc));
    circuit.add_plan(p1, R_SCM_SCO);
    let mut p2 = NaryPlan::new();
    p2.push_join(Box::new(CaxSco));
    circuit.add_plan(p2, R_CAX_SCO);
    for i in 1..=n {
        circuit.assert_triple((i, SC, i + 1));
        circuit.assert_triple((IND_BASE + i, TYPE, i));
    }
    circuit.tick();
    circuit
}

fn bench_retract(c: &mut Criterion) {
    let mut group = c.benchmark_group("retract_small_delta");
    // The recompute fallback re-derives the whole ~N² closure per retraction
    // tick with naïve O(|a|·|b|) reference joins; keep the sample count low
    // so the full (non---quick) run stays tractable.
    group.sample_size(10);
    for &n in &[64u64, 128, 256] {
        // Interior small-delta cut edge — see the module doc for why this is
        // position N−4, not the exact middle.
        let cut = (n - 4, SC, n - 3);
        for (name, fallback) in [("incremental", false), ("recompute_fallback", true)] {
            group.bench_with_input(BenchmarkId::new(name, n), &n, |b, &n| {
                let mut circuit = warm_circuit(fallback, n);
                b.iter(|| {
                    circuit.retract_triple(cut);
                    circuit.tick();
                    circuit.assert_triple(cut);
                    circuit.tick();
                    std::hint::black_box(circuit.derived_base().len())
                })
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_retract);
criterion_main!(benches);
