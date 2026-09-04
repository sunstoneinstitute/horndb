//! Cost-based join planning (SPEC-23 §5.5): structural routing, the cost
//! model's bounds, and the HDB-108 variable-ordering win.

use std::collections::BTreeSet;

use horndb_wcoj::cost::CostModel;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::JoinSpec;
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::stats::{SnapshotStats, Stats, ZeroStats};

fn v(i: u8) -> Term {
    Term::Var(Var(i))
}

fn pat(s: Term, p: u64, o: Term) -> TriplePattern {
    TriplePattern::new(s, Term::Bound(p), o)
}

fn triangle() -> Bgp {
    Bgp::new(vec![
        pat(v(0), 10, v(1)),
        pat(v(1), 10, v(2)),
        pat(v(2), 10, v(0)),
    ])
}

fn four_cycle() -> Bgp {
    Bgp::new(vec![
        pat(v(0), 10, v(1)),
        pat(v(1), 10, v(2)),
        pat(v(2), 10, v(3)),
        pat(v(3), 10, v(0)),
    ])
}

fn star(n: u8) -> Bgp {
    Bgp::new((1..=n).map(|i| pat(v(0), 10 + i as u64, v(i))).collect())
}

fn path(n: u8) -> Bgp {
    Bgp::new((0..n).map(|i| pat(v(i), 10, v(i + 1))).collect())
}

/// Random-ish graph: `n` subjects, every predicate 10..=16, 0-3 edges each.
fn source(n: u64) -> VecTripleSource {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut triples = Vec::new();
    for s in 0..n {
        for p in 10..=16 {
            for _ in 0..(rand() % 4) {
                triples.push(Triple::new(s, p, rand() % n));
            }
        }
    }
    VecTripleSource::from_triples(triples)
}

/// Every pattern exactly once, every WCOJ node non-empty with a var order
/// covering its variables, every hash join sharing a variable or being a
/// deliberate cross product of disconnected parts.
fn assert_valid(spec: &JoinSpec, bgp: &Bgp) {
    let covered = spec.patterns();
    let all: Vec<usize> = (0..bgp.patterns.len()).collect();
    assert_eq!(covered, all, "spec must cover each pattern once: {spec:?}");
    fn walk(spec: &JoinSpec, bgp: &Bgp) {
        match spec {
            JoinSpec::Scan { .. } => {}
            JoinSpec::Wcoj {
                patterns,
                var_order,
            } => {
                assert!(!patterns.is_empty());
                let sub = Bgp::new(patterns.iter().map(|&i| bgp.patterns[i]).collect());
                let want: BTreeSet<Var> = sub.variables().into_iter().collect();
                let got: BTreeSet<Var> = var_order.iter().copied().collect();
                assert_eq!(want, got, "var order must cover the node's vars");
                assert_eq!(var_order.len(), got.len(), "duplicate var in order");
            }
            JoinSpec::HashJoin { build, probe } => {
                walk(build, bgp);
                walk(probe, bgp);
            }
        }
    }
    walk(spec, bgp);
}

fn core(bgp: &Bgp, stats: &dyn Stats) -> Vec<usize> {
    let live: Vec<usize> = (0..bgp.patterns.len()).collect();
    CostModel::new(bgp, stats).cyclic_core(&live)
}

#[test]
fn gyo_reduces_acyclic_shapes_to_nothing() {
    let z = ZeroStats::new(0);
    assert!(core(&star(6), &z).is_empty());
    assert!(core(&path(5), &z).is_empty());
    assert_eq!(core(&triangle(), &z), vec![0, 1, 2]);
    assert_eq!(core(&four_cycle(), &z), vec![0, 1, 2, 3]);
    // Triangle with a tail: the tail is an ear, the triangle survives.
    let mut ps = triangle().patterns;
    ps.push(pat(v(2), 11, v(3)));
    assert_eq!(core(&Bgp::new(ps), &z), vec![0, 1, 2]);
}

#[test]
fn agm_bound_matches_fractional_edge_cover() {
    // One predicate with N triples: triangle N^1.5, 4-cycle N^2, 2-star N^2.
    let n = 1000u64;
    let src = VecTripleSource::from_triples((0..n).map(|i| Triple::new(i, 10, i + 1)).collect());
    let stats = SnapshotStats::from_source(&src);
    let bound = |bgp: &Bgp| {
        let mask = (1u64 << bgp.patterns.len()) - 1;
        CostModel::new(bgp, &stats).agm_bound(mask)
    };
    let nf = n as f64;
    assert!((bound(&triangle()) - nf.powf(1.5)).abs() < 1e-6);
    assert!((bound(&four_cycle()) - nf * nf).abs() < 1e-6);
    let two_star = Bgp::new(vec![pat(v(0), 10, v(1)), pat(v(0), 10, v(2))]);
    assert!((bound(&two_star) - nf * nf).abs() < 1e-6);
}

#[test]
fn uninformed_stats_route_structurally() {
    let bgp = star(3);
    let spec = Planner::default().choose(&bgp, &ZeroStats::new(0));
    assert_eq!(
        spec.as_whole_wcoj(&bgp),
        Some(&[Var(0), Var(1), Var(2), Var(3)][..])
    );
}

#[test]
fn cyclic_core_is_never_split() {
    let src = source(200);
    let stats = SnapshotStats::from_source(&src);
    for bgp in [triangle(), four_cycle()] {
        let spec = Planner::default().choose(&bgp, &stats);
        assert!(spec.as_whole_wcoj(&bgp).is_some(), "{spec:?}");
    }
    // Triangle plus two tails: whatever the split, the triangle stays one node.
    let mut ps = triangle().patterns;
    ps.push(pat(v(2), 11, v(3)));
    ps.push(pat(v(3), 12, v(4)));
    let bgp = Bgp::new(ps);
    let spec = Planner::default().choose(&bgp, &stats);
    assert_valid(&spec, &bgp);
    fn holds_triangle(spec: &JoinSpec) -> bool {
        match spec {
            JoinSpec::Wcoj { patterns, .. } => [0, 1, 2].iter().all(|i| patterns.contains(i)),
            JoinSpec::Scan { .. } => false,
            JoinSpec::HashJoin { build, probe } => holds_triangle(build) || holds_triangle(probe),
        }
    }
    assert!(holds_triangle(&spec), "{spec:?}");
}

#[test]
fn every_shape_yields_a_valid_spec() {
    let src = source(200);
    let stats = SnapshotStats::from_source(&src);
    let mut shapes = vec![star(6), path(6), star(3), triangle()];
    // Ground pattern alongside live ones, and a disconnected pair.
    let mut ps = star(2).patterns;
    ps.push(TriplePattern::new(
        Term::Bound(1),
        Term::Bound(10),
        Term::Bound(2),
    ));
    shapes.push(Bgp::new(ps));
    shapes.push(Bgp::new(vec![pat(v(0), 10, v(1)), pat(v(2), 11, v(3))]));
    // Past the DP threshold: the greedy path.
    shapes.push(path(12));
    for bgp in shapes {
        let spec = Planner::default().choose(&bgp, &stats);
        assert_valid(&spec, &bgp);
    }
}

#[test]
fn fixed_cutover_restores_stage1_rule() {
    let planner = Planner {
        fixed_cutover: Some(4),
        ..Planner::default()
    };
    let src = source(50);
    let stats = SnapshotStats::from_source(&src);
    assert!(planner
        .choose(&four_cycle(), &stats)
        .as_whole_wcoj(&four_cycle())
        .is_some());
    let spec = planner.choose(&star(2), &stats);
    assert!(matches!(spec, JoinSpec::HashJoin { .. }), "{spec:?}");
}

/// HDB-108: trainmarks q3. `?order` sits in four patterns and `?customer` in
/// three, so degree order starts at `?order` and walks every order before
/// the selective `:country :Norway` filter applies. The cost model must bind
/// `?customer` first.
#[test]
fn q3_shape_binds_selective_customer_before_order() {
    const PLACED_BY: u64 = 1;
    const CONTAINS: u64 = 2;
    const AMOUNT: u64 = 3;
    const STATUS: u64 = 4;
    const LABEL: u64 = 5;
    const COUNTRY: u64 = 6;
    const NORWAY: u64 = 7;
    let customers = 1_000u64;
    let products = 100u64;
    let orders = 20_000u64;
    let mut t = Vec::new();
    let mut state = 12345u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for c in 0..customers {
        let id = 1_000_000 + c;
        t.push(Triple::new(id, LABEL, 5_000_000 + c));
        // Twenty countries; Norway is one of them.
        t.push(Triple::new(id, COUNTRY, NORWAY + rand() % 20));
    }
    for p in 0..products {
        t.push(Triple::new(2_000_000 + p, LABEL, 6_000_000 + p));
    }
    for o in 0..orders {
        let id = 3_000_000 + o;
        t.push(Triple::new(id, PLACED_BY, 1_000_000 + rand() % customers));
        t.push(Triple::new(id, CONTAINS, 2_000_000 + rand() % products));
        t.push(Triple::new(id, AMOUNT, 7_000_000 + rand() % 5000));
        t.push(Triple::new(id, STATUS, 8_000_000 + rand() % 4));
    }
    let src = VecTripleSource::from_triples(t);
    let stats = SnapshotStats::from_source(&src);
    let (order, customer, product, amount, status, cname, pname) = (0, 1, 2, 3, 4, 5, 6);
    let bgp = Bgp::new(vec![
        pat(v(order), PLACED_BY, v(customer)),
        pat(v(order), CONTAINS, v(product)),
        pat(v(order), AMOUNT, v(amount)),
        pat(v(order), STATUS, v(status)),
        pat(v(customer), LABEL, v(cname)),
        TriplePattern::new(v(customer), Term::Bound(COUNTRY), Term::Bound(NORWAY)),
        pat(v(product), LABEL, v(pname)),
    ]);
    let spec = Planner::default().choose(&bgp, &stats);
    assert_valid(&spec, &bgp);
    let binds = spec.vars(&bgp);
    let at = |x: u8| binds.iter().position(|w| *w == Var(x)).unwrap();
    assert!(
        at(customer) < at(order),
        "binding order {binds:?} in {spec:?}"
    );
}

/// Planning cost is bounded at the DP's worst case: `MAX_DP_PATTERNS` (5)
/// patterns over four variables with a chord, so nearly every subset is
/// connected and gets costed. Wider BGPs go greedy (cheaper). The bound is
/// loose on purpose -- this guards against a blow-up, not a few microseconds
/// of nextest contention.
#[test]
fn dense_five_pattern_bgp_plans_fast() {
    let src = source(2_000);
    let stats = SnapshotStats::from_source(&src);
    let planner = Planner::default();
    let mut ps = four_cycle().patterns;
    ps.push(pat(v(0), 11, v(2)));
    let bgp = Bgp::new(ps);
    let _ = planner.choose(&bgp, &stats); // warm
    let started = std::time::Instant::now();
    let iters = 20;
    for _ in 0..iters {
        let _ = planner.choose(&bgp, &stats);
    }
    let per = started.elapsed() / iters;
    let bound = if cfg!(debug_assertions) {
        std::time::Duration::from_millis(50)
    } else {
        std::time::Duration::from_millis(2)
    };
    eprintln!("dense 5-pattern planning: {per:?} (bound {bound:?})");
    for k in [3u8, 5, 7, 10] {
        let bgp = star(k);
        let started = std::time::Instant::now();
        for _ in 0..iters {
            let _ = planner.choose(&bgp, &stats);
        }
        eprintln!("{k}-star planning: {:?}", started.elapsed() / iters);
    }
    assert!(
        per <= bound,
        "dense 5-pattern BGP planned in {per:?} > {bound:?}"
    );
}

/// Two vertex-disjoint triangles are two cyclic cores, not one: each stays
/// whole in one WCOJ node, and the plan is free to join across them.
#[test]
fn cores_are_per_connected_component() {
    let src = source(200);
    let stats = SnapshotStats::from_source(&src);
    let mut ps = triangle().patterns;
    ps.extend([
        pat(v(3), 11, v(4)),
        pat(v(4), 11, v(5)),
        pat(v(5), 11, v(3)),
    ]);
    let bgp = Bgp::new(ps);
    let spec = Planner::default().choose(&bgp, &stats);
    assert_valid(&spec, &bgp);
    fn holds(spec: &JoinSpec, want: &[usize]) -> bool {
        match spec {
            JoinSpec::Wcoj { patterns, .. } => want.iter().all(|i| patterns.contains(i)),
            JoinSpec::Scan { .. } => false,
            JoinSpec::HashJoin { build, probe } => holds(build, want) || holds(probe, want),
        }
    }
    assert!(holds(&spec, &[0, 1, 2]), "{spec:?}");
    assert!(holds(&spec, &[3, 4, 5]), "{spec:?}");
    // One core would force a single six-pattern node; two cores let the
    // cheaper plan (each triangle once, then a cross product) through.
    assert!(spec.as_whole_wcoj(&bgp).is_none(), "{spec:?}");
}

/// An empty BGP is the join identity: one solution with no bindings.
#[test]
fn empty_bgp_is_the_join_identity() {
    use horndb_wcoj::cancel::CancelToken;
    use horndb_wcoj::executor::Executor;
    let src = source(50);
    let stats = SnapshotStats::from_source(&src);
    let bgp = Bgp::new(vec![]);
    let spec = Planner::default().choose(&bgp, &stats);
    assert!(spec.patterns().is_empty(), "{spec:?}");
    for stats in [&stats as &dyn Stats, &ZeroStats::new(0)] {
        let rows: usize =
            Executor::for_bgp(&src, &bgp, &Planner::default(), stats, CancelToken::new())
                .map(|b| b.unwrap().num_rows())
                .sum();
        assert_eq!(rows, 1);
    }
}
