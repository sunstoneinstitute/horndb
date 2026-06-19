//! Fork-A crosswalk closure tests (TASKS.md #12).

use horndb_closure::crosswalk::{CrosswalkEdge, CrosswalkError, CrosswalkGraph};
use horndb_closure::grb::init_once;
use horndb_closure::metrics::CarrierShape;

fn edge(src: u64, dst: u64, confidence: f64) -> CrosswalkEdge {
    CrosswalkEdge {
        src,
        dst,
        confidence,
    }
}

fn pair_conf(graph: &CrosswalkGraph, from: u64, to: u64) -> Option<f64> {
    graph.best_confidence(from, to).unwrap()
}

/// Assert an optional confidence equals `expected` within FP tolerance — the
/// `(max, ×)` product of confidences accumulates rounding, so exact `==` on a
/// chained product is wrong.
fn assert_conf(actual: Option<f64>, expected: f64) {
    match actual {
        Some(w) => assert!((w - expected).abs() < 1e-12, "expected {expected}, got {w}"),
        None => panic!("expected Some({expected}), got None"),
    }
}

/// Best-confidence path across a two-hop crosswalk chain with a weaker direct
/// shortcut. Dictionary IDs are sparse/non-contiguous to exercise the dense
/// renumbering (SPEC-05 F7).
///
///   100 --0.9--> 200 --0.8--> 300 ,  100 --0.5--> 300
/// best(100, 300) = max(0.9*0.8, 0.5) = 0.72.
#[test]
fn best_confidence_two_hop_beats_direct_shortcut() {
    init_once().unwrap();
    let g = CrosswalkGraph::from_edges(&[
        edge(100, 200, 0.9),
        edge(200, 300, 0.8),
        edge(100, 300, 0.5),
    ])
    .unwrap();

    assert_eq!(g.n(), 3);

    let (pairs, metrics) = g.best_confidence_closure().unwrap();
    assert_eq!(metrics.n, 3);
    assert_eq!(metrics.input_nnz, 3);
    assert_eq!(metrics.carrier, CarrierShape::Scalar);

    // The two-hop chain (0.72) beats the direct 0.5 edge.
    let w = pairs
        .iter()
        .find(|p| p.src == 100 && p.dst == 300)
        .expect("100 -> 300 reachable");
    assert!((w.confidence - 0.72).abs() < 1e-12, "got {}", w.confidence);

    // Result coordinates are mapped back to the original dictionary IDs.
    assert_conf(pair_conf(&g, 100, 200), 0.9);
    assert_conf(pair_conf(&g, 200, 300), 0.8);
    assert_conf(pair_conf(&g, 100, 300), 0.72);
}

/// A GTIO/SKOS-shaped crosswalk: two source vocab concepts mapped into a target
/// vocab via `skos:*Match` edges of differing confidence, plus a target-side
/// `broader` hierarchy. Resolution must pick the strongest *chain*.
///
///   src:A --exactMatch(0.99)--> tgt:X --broader(0.7)--> tgt:Y
///   src:A --closeMatch(0.6)---> tgt:Y                      (direct, weaker)
/// best(A, Y) = max(0.99*0.7, 0.6) = 0.693.
#[test]
fn skos_crosswalk_resolution() {
    init_once().unwrap();
    // Dictionary IDs: A=10, B=11, X=20, Y=21, Z=22.
    let g = CrosswalkGraph::from_edges(&[
        edge(10, 20, 0.99), // A exactMatch X
        edge(20, 21, 0.70), // X broader Y
        edge(10, 21, 0.60), // A closeMatch Y (weaker direct)
        edge(11, 22, 0.85), // B exactMatch Z
        edge(22, 21, 0.50), // Z broader Y
    ])
    .unwrap();

    assert_conf(pair_conf(&g, 10, 21), 0.99 * 0.70); // 0.693 chain wins over 0.60
    assert_conf(pair_conf(&g, 10, 20), 0.99);
    assert_conf(pair_conf(&g, 11, 21), 0.85 * 0.50); // B -> Z -> Y = 0.425
                                                     // No path from B to X.
    assert_eq!(pair_conf(&g, 11, 20), None);
}

/// Unknown IDs and self-pairs resolve to `None` (identity is not in the
/// closure).
#[test]
fn unknown_and_identity_pairs_are_none() {
    init_once().unwrap();
    let g = CrosswalkGraph::from_edges(&[edge(1, 2, 0.9)]).unwrap();
    assert_conf(pair_conf(&g, 1, 2), 0.9);
    assert_eq!(pair_conf(&g, 1, 1), None, "identity not in closure");
    assert_eq!(pair_conf(&g, 9, 1), None, "unknown src");
    assert_eq!(pair_conf(&g, 1, 9), None, "unknown dst");
}

/// Duplicate `(src, dst)` edges keep the strongest confidence.
#[test]
fn duplicate_edges_keep_max_confidence() {
    init_once().unwrap();
    let g =
        CrosswalkGraph::from_edges(&[edge(1, 2, 0.4), edge(1, 2, 0.8), edge(1, 2, 0.6)]).unwrap();
    assert_eq!(g.n(), 2);
    assert_conf(pair_conf(&g, 1, 2), 0.8);
}

/// Empty graph: zero nodes, empty closure, well-formed metrics.
#[test]
fn empty_graph() {
    init_once().unwrap();
    let g = CrosswalkGraph::from_edges(&[]).unwrap();
    assert_eq!(g.n(), 0);
    let (pairs, metrics) = g.best_confidence_closure().unwrap();
    assert!(pairs.is_empty());
    assert_eq!(metrics.n, 0);
    assert_eq!(metrics.input_nnz, 0);
    assert_eq!(metrics.iterations_to_fixpoint, 0);
}

/// `from_edges` self-initialises GraphBLAS — no explicit `init_once()` needed
/// (the documented high-level entry point works standalone). `init_once` is
/// idempotent and may already have run in this shared test binary, but this at
/// least exercises the in-method call.
#[test]
fn from_edges_self_initialises() {
    // Deliberately NO `init_once()` here.
    let g = CrosswalkGraph::from_edges(&[edge(1, 2, 0.5)]).unwrap();
    assert_eq!(g.n(), 2);
    assert_conf(pair_conf(&g, 1, 2), 0.5);
}

/// Out-of-contract confidences are rejected at the boundary so the closure
/// stays sound (a `> 1` weight could diverge over a cycle and return
/// cap-dependent answers; `≤ 0` and non-finite are not confidences).
#[test]
fn invalid_confidence_is_rejected() {
    init_once().unwrap();
    for bad in [1.5_f64, 0.0, -0.1, f64::NAN, f64::INFINITY] {
        match CrosswalkGraph::from_edges(&[edge(7, 8, bad)]) {
            Ok(_) => panic!("expected InvalidConfidence for {bad}, got Ok"),
            Err(CrosswalkError::InvalidConfidence {
                src,
                dst,
                confidence,
            }) => {
                assert_eq!((src, dst), (7, 8));
                assert!(
                    confidence.is_nan() == bad.is_nan()
                        && (confidence.is_nan() || confidence == bad),
                    "reported confidence {confidence} != offending {bad}"
                );
            }
            Err(other) => panic!("expected InvalidConfidence for {bad}, got {other:?}"),
        }
    }
    // The boundary `1.0` (certain match) is valid.
    assert_conf(
        pair_conf(
            &CrosswalkGraph::from_edges(&[edge(7, 8, 1.0)]).unwrap(),
            7,
            8,
        ),
        1.0,
    );
}
