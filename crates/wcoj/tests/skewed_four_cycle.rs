//! Correctness gate for the canonical WCOJ-win 4-cycle graph
//! ([`SyntheticGraph::skewed_four_cycle`]), the shape behind SPEC-03
//! acceptance criterion #2 (`benches/four_cycle.rs`).
//!
//! The ≥10× *speed* gate itself is a manual/nightly criterion bench (it is a
//! wall-clock ratio and would be flaky in CI). What we lock down here is the
//! thing that must never silently break: that both executors compute the
//! *correct* 4-cycle result over the skewed graph, checked against an
//! independent brute-force count. If a future change to the generator or the
//! executors quietly altered the answer set, the headline bench ratio would
//! be meaningless — this test prevents that.

use std::collections::{HashMap, HashSet};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::synthetic::{SkewedFourCycle, SyntheticGraph};
use horndb_wcoj::source::vec_source::VecTripleSource;

/// A small instance of the win-case shape — same topology as the bench, sized
/// to run in milliseconds.
const SMALL: SkewedFourCycle = SkewedFourCycle {
    sources: 50,
    a_out: 2,
    hubs: 10,
    hub_out: 4,
    bulk_sinks: 50,
    close_sources: 4,
    close_sinks: 2,
    predicate: 10,
    seed: 0xCAFE_F00D,
};

fn four_cycle_bgp(p: u64) -> Bgp {
    Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ])
}

/// Count 4-walks `a→b→c→d→a` (no distinctness requirement, matching BGP join
/// semantics) directly from the edge list — independent of either executor.
fn brute_force_four_cycles(edges: &[Triple]) -> u64 {
    let mut out: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut edge: HashSet<(u64, u64)> = HashSet::new();
    for t in edges {
        out.entry(t.s).or_default().push(t.o);
        edge.insert((t.s, t.o));
    }
    let empty: Vec<u64> = Vec::new();
    let mut count = 0u64;
    for (&a, bs) in &out {
        for &b in bs {
            for &c in out.get(&b).unwrap_or(&empty) {
                for &d in out.get(&c).unwrap_or(&empty) {
                    if edge.contains(&(d, a)) {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

fn wcoj_rows(src: &VecTripleSource, bgp: &Bgp) -> u64 {
    let plan = ExecutionPlan {
        kind: PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1), Var(2), Var(3)],
    };
    let exec = WcojExecutor::new(src, bgp, &plan, CancelToken::new());
    exec.into_iter().map(|b| b.unwrap().num_rows() as u64).sum()
}

fn binary_rows(src: &VecTripleSource, bgp: &Bgp) -> u64 {
    let exec = BinaryHashExecutor::new(
        src,
        bgp,
        vec![Var(0), Var(1), Var(2), Var(3)],
        CancelToken::new(),
    );
    exec.into_iter().map(|b| b.unwrap().num_rows() as u64).sum()
}

#[test]
fn skewed_four_cycle_both_executors_match_brute_force() {
    let edges = SyntheticGraph::skewed_four_cycle_edges(&SMALL);
    let expected = brute_force_four_cycles(&edges);
    assert!(
        expected > 0,
        "win-case graph must contain at least one 4-cycle"
    );

    let src = VecTripleSource::from_triples(edges);
    let bgp = four_cycle_bgp(SMALL.predicate);

    let wcoj = wcoj_rows(&src, &bgp);
    let binary = binary_rows(&src, &bgp);

    assert_eq!(
        wcoj, expected,
        "WCOJ disagrees with brute-force 4-cycle count"
    );
    assert_eq!(
        binary, expected,
        "binary-hash disagrees with brute-force 4-cycle count"
    );
}

#[test]
fn skewed_four_cycle_edge_layout_is_deterministic() {
    let a = SyntheticGraph::skewed_four_cycle_edges(&SMALL);
    let b = SyntheticGraph::skewed_four_cycle_edges(&SMALL);
    assert_eq!(a, b, "generator must be deterministic for a fixed seed");

    // Bulk of the graph is the B→C fan: middles * hubs edges, where
    // middles = sources * a_out. Everything else (the A→B stem, the C→D
    // blow-up, and the thin closure) is comparatively small, so total edges
    // are dominated by — and at least as large as — the fan.
    let fan = SMALL.sources * SMALL.a_out * SMALL.hubs;
    assert!(
        a.len() as u64 >= fan,
        "expected at least the B→C fan ({fan} edges), got {}",
        a.len()
    );
    // Single predicate throughout.
    assert!(a.iter().all(|t| t.p == SMALL.predicate));
}
