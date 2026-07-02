use std::sync::Arc;
use std::time::{Duration, Instant};

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::error::WcojError;
use horndb_wcoj::executor::Executor;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::synthetic::SyntheticGraph;

#[test]
fn cancellation_returns_within_100ms() {
    // Pay SIMD kernel calibration up front so it is not on the cancellation
    // critical path. Calibration is a one-time startup cost (production primes
    // it via `horndb_simd::init()` at server startup), not per-cancellation
    // latency. On a CPU absent from the known-CPU table the kernels calibrate
    // *lazily* on first use — which, without this prime, happens mid-descent
    // between the executor's depth-0 cancellation checks. In a debug build that
    // lazy calibration takes ~350ms, so it would spuriously blow the <100ms
    // budget on CI (an unlisted x86 runner built without `--release`) even
    // though real cancellation latency is ~35ms. See crates/simd/AGENTS.md.
    horndb_simd::init();

    // Build a graph dense enough that the optimized 4-cycle executor
    // is still doing work well after the 10ms cancel deadline. A
    // 250K-vertex × 4-out-edge cyclic graph is the same shape the
    // four_cycle bench uses; full execution there is multiple seconds
    // on release, so cancel reliably catches it mid-flight.
    let p = 10u64;
    let src = Arc::new(SyntheticGraph::cyclic(250_000, 4, p, 0xCAFE_F00D));
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(p), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(1)), Term::Bound(p), Term::Var(Var(2))),
        TriplePattern::new(Term::Var(Var(2)), Term::Bound(p), Term::Var(Var(3))),
        TriplePattern::new(Term::Var(Var(3)), Term::Bound(p), Term::Var(Var(0))),
    ]);
    let token = CancelToken::new();
    let token_clone = token.clone();
    let planner = Planner::default();
    let src_ref: &SyntheticGraph = &src;

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
    assert!(
        matches!(last_err, Some(WcojError::Cancelled)),
        "got {last_err:?}"
    );
}
