use std::sync::Arc;
use std::time::{Duration, Instant};

use reasoner_wcoj::cancel::CancelToken;
use reasoner_wcoj::error::WcojError;
use reasoner_wcoj::executor::Executor;
use reasoner_wcoj::ids::Triple;
use reasoner_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use reasoner_wcoj::planner::Planner;
use reasoner_wcoj::source::vec_source::VecTripleSource;

#[test]
fn cancellation_returns_within_100ms() {
    // Build a synthetic graph large enough to keep the executor busy.
    let p = 10u64;
    let mut triples = Vec::new();
    for s in 0..10_000u64 {
        triples.push(Triple::new(s, p, (s + 1) % 10_000));
    }
    let src = Arc::new(VecTripleSource::from_triples(triples));
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let token = CancelToken::new();
    let token_clone = token.clone();
    let planner = Planner::default();
    let src_ref: &VecTripleSource = &src;

    // Cancel after 10 ms from another thread.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        token_clone.cancel();
    });

    let start = Instant::now();
    let exec = Executor::for_bgp(src_ref, &bgp, &planner, token.clone());
    let mut last_err = None;
    for item in exec {
        if let Err(e) = item {
            last_err = Some(e);
            break;
        }
    }
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(100), "took {elapsed:?}");
    assert!(matches!(last_err, Some(WcojError::Cancelled)), "got {:?}", last_err);
}
