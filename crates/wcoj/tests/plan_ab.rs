//! Same-process A/B (SPEC-23 §7.4 "no query regresses beyond a set
//! tolerance"): for each SPEC-03 acceptance shape with a distinct predicate
//! per edge, run the whole-BGP leapfrog in plain degree order and the
//! cost-based plan on the same source, best of three, and require the plan
//! to stay within `TOLERANCE` of the degree order. Also bounds planning
//! time. Laptop numbers printed here are for the A/B only — never record
//! them in `docs/benchmarks.md`.

use std::time::{Duration, Instant};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::Executor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{degree_order, JoinSpec};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::stats::SnapshotStats;

/// Planned runtime may exceed degree-order runtime by this factor plus a
/// floor of half the baseline, never under `MIN_FLOOR` (timer noise).
const TOLERANCE: f64 = 1.5;
const MIN_FLOOR: Duration = Duration::from_millis(1);
const TYPE: u64 = 900;

fn n_subjects() -> u64 {
    if cfg!(debug_assertions) {
        3_000
    } else {
        20_000
    }
}

fn v(i: u8) -> Term {
    Term::Var(Var(i))
}

fn pat(s: Term, p: u64, o: Term) -> TriplePattern {
    TriplePattern::new(s, Term::Bound(p), o)
}

/// `n` subjects; each predicate 1..=8 gives every subject 0–3 random edges;
/// `rdf:type`-like predicate 900 gives 90% of subjects class 1, the rest 2–10.
fn graph(n: u64) -> VecTripleSource {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut t = Vec::new();
    for s in 0..n {
        for p in 1..=8u64 {
            for _ in 0..(rand() % 4) {
                t.push(Triple::new(s, p, rand() % n));
            }
        }
        let class = if rand() % 10 == 0 { 2 + rand() % 9 } else { 1 };
        t.push(Triple::new(s, TYPE, 10_000 + class));
    }
    VecTripleSource::from_triples(t)
}

fn shapes() -> Vec<(&'static str, Bgp)> {
    let star = |k: u8| Bgp::new((1..=k).map(|i| pat(v(0), i as u64, v(i))).collect());
    let mut star_tail = star(3).patterns;
    star_tail.push(pat(v(3), 4, v(4)));
    vec![
        (
            "triangle",
            Bgp::new(vec![
                pat(v(0), 1, v(1)),
                pat(v(1), 2, v(2)),
                pat(v(2), 3, v(0)),
            ]),
        ),
        (
            "4-cycle",
            Bgp::new(vec![
                pat(v(0), 1, v(1)),
                pat(v(1), 2, v(2)),
                pat(v(2), 3, v(3)),
                pat(v(3), 4, v(0)),
            ]),
        ),
        (
            "4-path",
            Bgp::new((0..4).map(|i| pat(v(i), i as u64 + 1, v(i + 1))).collect()),
        ),
        ("4-star", star(4)),
        ("5-star", star(5)),
        ("star+tail", Bgp::new(star_tail)),
        (
            "knows-2-path+type",
            Bgp::new(vec![
                pat(v(0), 1, v(1)),
                pat(v(1), 1, v(2)),
                TriplePattern::new(v(0), Term::Bound(TYPE), Term::Bound(10_003)),
            ]),
        ),
    ]
}

/// The HDB-108 trainmarks q3 shape on its own customer/order/product graph.
fn q3() -> (VecTripleSource, Bgp) {
    const PLACED_BY: u64 = 1;
    const CONTAINS: u64 = 2;
    const AMOUNT: u64 = 3;
    const STATUS: u64 = 4;
    const LABEL: u64 = 5;
    const COUNTRY: u64 = 6;
    const NORWAY: u64 = 7;
    let (customers, products, orders) = (1_000u64, 100u64, n_subjects());
    let mut state = 12345u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut t = Vec::new();
    for c in 0..customers {
        t.push(Triple::new(1_000_000 + c, LABEL, 5_000_000 + c));
        t.push(Triple::new(1_000_000 + c, COUNTRY, NORWAY + rand() % 20));
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
    (VecTripleSource::from_triples(t), bgp)
}

/// One variable, all objects bound: `?x a :Person . ?x :country :NO .
/// ?x :status :Active . ?x :gender :F` over 25× the usual subject count.
/// Every pattern is a filter on the same variable, so the only plan question
/// is intersect-in-one-node versus hash-join the filters.
fn attr_star() -> (VecTripleSource, Bgp) {
    const COUNTRY: u64 = 21;
    const STATUS: u64 = 22;
    const GENDER: u64 = 23;
    let n = n_subjects() * 25;
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut t = Vec::with_capacity(n as usize * 4);
    for s in 0..n {
        let class = if rand() % 10 == 0 { 2 + rand() % 9 } else { 1 };
        t.push(Triple::new(s, TYPE, 10_000 + class));
        t.push(Triple::new(s, COUNTRY, 20_000 + rand() % 20));
        t.push(Triple::new(s, STATUS, 30_000 + rand() % 4));
        t.push(Triple::new(s, GENDER, 40_000 + rand() % 2));
    }
    let bgp = Bgp::new(vec![
        TriplePattern::new(v(0), Term::Bound(TYPE), Term::Bound(10_001)),
        TriplePattern::new(v(0), Term::Bound(COUNTRY), Term::Bound(20_000)),
        TriplePattern::new(v(0), Term::Bound(STATUS), Term::Bound(30_000)),
        TriplePattern::new(v(0), Term::Bound(GENDER), Term::Bound(40_000)),
    ]);
    (VecTripleSource::from_triples(t), bgp)
}

fn run(src: &VecTripleSource, bgp: &Bgp, spec: &JoinSpec) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut rows = 0;
    for _ in 0..3 {
        let t = Instant::now();
        rows = Executor::for_spec(src, bgp, spec, CancelToken::new())
            .map(|b| b.unwrap().num_rows())
            .sum();
        best = best.min(t.elapsed());
    }
    (best, rows)
}

fn ab(name: &str, src: &VecTripleSource, bgp: &Bgp, failures: &mut Vec<String>) {
    let stats = SnapshotStats::from_source(src);
    let t = Instant::now();
    let planned = Planner::default().choose(bgp, &stats);
    let plan_time = t.elapsed();
    let degree = JoinSpec::Wcoj {
        patterns: (0..bgp.patterns.len()).collect(),
        var_order: degree_order(bgp),
    };
    let (td, rd) = run(src, bgp, &degree);
    let (tp, rp) = run(src, bgp, &planned);
    let kind = match planned.as_whole_wcoj(bgp) {
        Some(order) => format!("wcoj {order:?}"),
        None => format!("hybrid {planned:?}"),
    };
    println!(
        "{name:>18} | degree {:>8.1} ms | planned {:>8.1} ms | x{:.2} | plan {:>6} us | {rd} rows | {kind}",
        td.as_secs_f64() * 1e3,
        tp.as_secs_f64() * 1e3,
        tp.as_secs_f64() / td.as_secs_f64(),
        plan_time.as_micros(),
    );
    assert_eq!(rd, rp, "{name}: planned row count differs");
    if tp > td.mul_f64(TOLERANCE) + td.mul_f64(0.5).max(MIN_FLOOR) {
        failures.push(format!("{name}: planned {tp:?} vs degree {td:?}"));
    }
}

#[test]
fn planned_within_tolerance_of_degree_order() {
    let src = graph(n_subjects());
    let mut failures = Vec::new();
    for (name, bgp) in shapes() {
        ab(name, &src, &bgp, &mut failures);
    }
    let (src, bgp) = q3();
    ab("q3", &src, &bgp, &mut failures);
    let (src, bgp) = attr_star();
    ab("attr-star", &src, &bgp, &mut failures);
    assert!(failures.is_empty(), "regressions: {failures:#?}");
}
