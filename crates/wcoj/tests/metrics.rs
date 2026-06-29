//! Verify that WCOJ BatchIter emits per-query metrics on Drop.

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::ExecutionPlan;
use horndb_wcoj::source::vec_source::VecTripleSource;

#[test]
fn wcoj_batchiter_emits_per_query_metrics() {
    // Triangle join: (?a, p, ?b)(?b, p, ?c)(?c, p, ?a) — copied from wcoj_smoke.rs
    let p = 10u64;
    let triples = vec![
        Triple::new(1, p, 2),
        Triple::new(2, p, 3),
        Triple::new(3, p, 1),
        Triple::new(1, p, 4),
    ];
    let src = VecTripleSource::from_triples(triples);
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order: vec![Var(0), Var(1), Var(2)],
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    // Consume the iterator fully — Drop fires on the BatchIter here.
    let _batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();

    // Drop has now fired; metrics should be recorded.
    let text = horndb_metrics::encode_metrics();
    assert!(
        text.contains("horndb_wcoj_peak_iterators"),
        "missing horndb_wcoj_peak_iterators:\n{text}"
    );
    assert!(
        text.contains("horndb_wcoj_seeks_per_query"),
        "missing horndb_wcoj_seeks_per_query:\n{text}"
    );
    assert!(
        text.contains("horndb_wcoj_iterations_per_query"),
        "missing horndb_wcoj_iterations_per_query:\n{text}"
    );
    // The triangle has 3 patterns → 3 iters → peak_iterators count >= 1
    let count = parse_histogram_count(&text, "horndb_wcoj_peak_iterators");
    assert!(
        count >= 1,
        "expected >= 1 peak_iterators observation, got {count}:\n{text}"
    );
}

/// Parse the `_count` line of a histogram from OpenMetrics text.
fn parse_histogram_count(text: &str, name: &str) -> u64 {
    let count_key = format!("{name}_count");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(&count_key) {
            if let Some(v) = rest.split_whitespace().next() {
                if let Ok(n) = v.parse::<u64>() {
                    return n;
                }
            }
        }
    }
    0
}
