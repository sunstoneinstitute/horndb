//! Correctness tests for the valued `(max, ×)` closure and its metrics
//! instrumentation (TASKS.md #11).

use horndb_closure::grb::{init_once, UserSemiring, ValuedMatrix};
use horndb_closure::metrics::{valued_transitive_closure, CarrierShape, ValuedKernel};

fn weight_of(edges: &[(u64, u64, f64)], r: u64, c: u64) -> Option<f64> {
    edges
        .iter()
        .find(|(a, b, _)| *a == r && *b == c)
        .map(|(_, _, w)| *w)
}

/// Best-confidence path on a small DAG:
///   0 --0.9--> 1 --0.8--> 2 ,  0 --0.5--> 2
/// Closure(0,2) = max(0.9*0.8, 0.5) = 0.72.
#[test]
fn valued_closure_best_confidence_path() {
    init_once().unwrap();
    let m = ValuedMatrix::from_weighted_edges(3, &[(0, 1, 0.9), (1, 2, 0.8), (0, 2, 0.5)]).unwrap();

    let (star, metrics) = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
    let edges = star.extract_weighted_edges().unwrap();

    assert_eq!(metrics.n, 3);
    assert_eq!(metrics.input_nnz, 3);
    assert_eq!(metrics.carrier, CarrierShape::Scalar);
    assert!(metrics.iterations_to_fixpoint >= 1);

    assert_eq!(weight_of(&edges, 0, 1), Some(0.9));
    assert_eq!(weight_of(&edges, 1, 2), Some(0.8));
    // The two-hop path beats the direct 0.5 edge.
    let w02 = weight_of(&edges, 0, 2).expect("0->2 must be reachable");
    assert!((w02 - 0.72).abs() < 1e-12, "expected 0.72, got {w02}");
}

/// Regression: the closure must keep iterating while *weights* still improve,
/// even when the reachable *support* (nnz) has already stabilised. Here every
/// reachable pair already exists after one hop (the graph is "complete" in
/// support via direct shortcut edges), but the best-confidence path `0→1→2→3`
/// (0.9·0.9·0.9 = 0.729) only beats the direct `0→3` shortcut (0.5) after the
/// *third* MxM. A naive nnz-only termination would stop early and report the
/// wrong weight for `0→3`.
#[test]
fn valued_closure_continues_on_weight_only_improvement() {
    init_once().unwrap();
    let edges = vec![
        // The "real" high-confidence chain.
        (0, 1, 0.9),
        (1, 2, 0.9),
        (2, 3, 0.9),
        // Direct + 2-hop shortcuts that already populate the support, so nnz
        // stops growing immediately, but with *lower* weight than the chain.
        (0, 2, 0.5),
        (0, 3, 0.5),
        (1, 3, 0.5),
    ];
    let m = ValuedMatrix::from_weighted_edges(4, &edges).unwrap();

    for kernel in [ValuedKernel::Builtin, ValuedKernel::Udf] {
        let (star, _metrics) = valued_transitive_closure(&m, kernel).unwrap();
        let out = star.extract_weighted_edges().unwrap();
        let w03 = weight_of(&out, 0, 3).expect("0->3 reachable");
        // 0.9^3 = 0.729 must win over every shorter shortcut.
        assert!(
            (w03 - 0.729).abs() < 1e-12,
            "{kernel:?}: best-confidence 0->3 should be 0.729 (the 3-hop chain), got {w03}"
        );
        let w02 = weight_of(&out, 0, 2).expect("0->2 reachable");
        assert!(
            (w02 - 0.81).abs() < 1e-12,
            "{kernel:?}: best-confidence 0->2 should be 0.81 (the 2-hop chain), got {w02}"
        );
    }
}

/// The built-in FactoryKernel and the user-defined-op generic kernel must
/// produce bit-identical closures — they only differ in *speed*, which is the
/// whole point of the readiness metric.
#[test]
fn builtin_and_udf_kernels_agree() {
    init_once().unwrap();
    let edges = vec![
        (0, 1, 0.9),
        (1, 2, 0.8),
        (2, 3, 0.95),
        (0, 3, 0.4),
        (1, 3, 0.6),
        (3, 4, 0.7),
    ];
    let m = ValuedMatrix::from_weighted_edges(5, &edges).unwrap();

    let (a, ma) = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
    let (b, mb) = valued_transitive_closure(&m, ValuedKernel::Udf).unwrap();

    let ea = a.extract_weighted_edges().unwrap();
    let eb = b.extract_weighted_edges().unwrap();
    assert_eq!(ea.len(), eb.len(), "kernels disagree on nnz");
    for ((ra, ca, wa), (rb, cb, wb)) in ea.iter().zip(eb.iter()) {
        assert_eq!((ra, ca), (rb, cb), "coordinate mismatch");
        assert!((wa - wb).abs() < 1e-12, "weight mismatch: {wa} vs {wb}");
    }
    assert_eq!(ma.closure_nnz, mb.closure_nnz);
    assert_eq!(ma.iterations_to_fixpoint, mb.iterations_to_fixpoint);
}

/// A UDF `(max, ×)` multiply must remain valid after the borrowed
/// `UserSemiring` is dropped — `mxm_max_times_udf` materialises the result
/// before returning, so the pending nonblocking op cannot reference freed
/// op/monoid handles. Reading the result *after* the drop would be a
/// use-after-free if the multiply were left pending.
#[test]
fn udf_mxm_survives_semiring_drop() {
    init_once().unwrap();
    let a = ValuedMatrix::from_weighted_edges(3, &[(0, 1, 0.9), (1, 2, 0.8)]).unwrap();
    let b = ValuedMatrix::from_weighted_edges(3, &[(0, 1, 0.5), (1, 2, 0.5)]).unwrap();

    let c = {
        let semiring = UserSemiring::max_times_fp64().unwrap();
        a.mxm_max_times_udf(&b, &semiring).unwrap()
        // `semiring` drops here, freeing its ops/monoid.
    };

    // If the multiply were still pending, this read would touch freed handles.
    let edges = c.extract_weighted_edges().unwrap();
    // a*b over (max,×): (0,2) = a[0,1]*b[1,2] = 0.9*0.5 = 0.45.
    assert_eq!(weight_of(&edges, 0, 2), Some(0.9 * 0.5));
}

/// `reduce_sum` totals the accumulated confidence mass.
#[test]
fn valued_reduce_sum() {
    init_once().unwrap();
    let m =
        ValuedMatrix::from_weighted_edges(4, &[(0, 1, 0.5), (1, 2, 0.25), (2, 3, 0.125)]).unwrap();
    let s = m.reduce_sum().unwrap();
    assert!((s - 0.875).abs() < 1e-12, "expected 0.875, got {s}");
    assert_eq!(ValuedMatrix::new(4).unwrap().reduce_sum().unwrap(), 0.0);
}

/// Empty input → empty closure, zero iterations, well-formed metrics.
#[test]
fn valued_closure_empty() {
    init_once().unwrap();
    let m = ValuedMatrix::new(8).unwrap();
    let (star, metrics) = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
    assert_eq!(star.nvals().unwrap(), 0);
    assert_eq!(metrics.n, 8);
    assert_eq!(metrics.input_nnz, 0);
    assert_eq!(metrics.closure_nnz, 0);
    assert_eq!(metrics.iterations_to_fixpoint, 0);
    assert_eq!(metrics.density, 0.0);
    assert!(metrics.frontier_nnz_per_iter.is_empty());
}

/// On an n-chain the closure has n(n-1)/2 reachable pairs and density/share
/// metrics are well-formed.
#[test]
fn valued_closure_chain_metrics() {
    init_once().unwrap();
    let n: u64 = 50;
    let edges: Vec<(u64, u64, f64)> = (0..n - 1).map(|i| (i, i + 1, 0.99)).collect();
    let m = ValuedMatrix::from_weighted_edges(n, &edges).unwrap();

    let (_star, metrics) = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
    assert_eq!(metrics.n, n);
    assert_eq!(metrics.input_nnz, n - 1);
    assert_eq!(metrics.closure_nnz, n * (n - 1) / 2);
    assert!(metrics.density > 0.0 && metrics.density < 1.0);
    assert!(metrics.mxm_share() >= 0.0 && metrics.mxm_share() <= 1.0);
    assert_eq!(
        metrics.iterations_to_fixpoint as usize,
        metrics.frontier_nnz_per_iter.len()
    );
    assert!(metrics.total_frontier_work() > 0);
}
