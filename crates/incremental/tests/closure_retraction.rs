//! SPEC-06 F6 end-to-end — closure-path retraction through the `Circuit`.
//!
//! A `TransitiveClosureRule` for one predicate, no rule plans. We assert a
//! transitive chain, tick, and confirm the inferred transitive edges land in
//! `derived_base` as `ClosureInferred`. Then we retract a base edge and tick,
//! confirming that closure edges which lost ALL support are withdrawn (their
//! `derived_base` multiplicity returns to 0 and a negative `ClosureInferred`
//! record appears on the feed), while edges still reachable by another path
//! persist. Finally we re-assert and confirm the closure re-derives.

use horndb_incremental::{Circuit, DerivationKind, TransitiveClosureRule};

const P: u64 = 100;

/// Drain every record currently queued on the receiver.
fn drain(rx: &horndb_incremental::ChangeFeedRx) -> Vec<horndb_incremental::DeltaRecord> {
    let mut out = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        out.push(rec);
    }
    out
}

/// (a) Chain break in the middle withdraws downstream pairs; the upstream
/// direct edge survives. A negative `ClosureInferred` is published for a
/// withdrawn edge. (c) Re-asserting the broken edge re-derives the closure.
#[test]
fn chain_break_withdraws_downstream_and_reassert_redrives() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Chain 1 -> 2 -> 3 -> 4.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    // Direct edges are asserted (not in derived_base); transitive edges are
    // ClosureInferred and materialized in derived_base.
    for &t in &[(1, P, 3), (1, P, 4), (2, P, 4)] {
        assert_eq!(
            circuit.derived_base().get(&t),
            1,
            "transitive edge {t:?} must be ClosureInferred"
        );
    }

    // Subscribe BEFORE the retraction tick so we observe its feed records.
    let rx = circuit.subscribe();

    // Break the chain in the middle: retract (2,P,3). Remaining base edges
    // {(1,2),(3,4)} close to just themselves; every pair that crossed node 2/3
    // loses support: (1,3),(1,4),(2,4),(2,3-direct).
    circuit.retract_triple((2, P, 3));
    circuit.tick();

    for &t in &[(1, P, 3), (1, P, 4), (2, P, 4)] {
        assert_eq!(
            circuit.derived_base().get(&t),
            0,
            "transitive edge {t:?} must be withdrawn after the chain break"
        );
    }
    // The surviving direct edges remain asserted; their reflexive/own presence
    // is unaffected. (1,P,2) and (3,P,4) are asserted, not derived.
    assert_eq!(circuit.derived_base().get(&(1, P, 2)), 0);
    assert_eq!(circuit.derived_base().get(&(3, P, 4)), 0);

    // A negative ClosureInferred must appear for a withdrawn edge.
    let records = drain(&rx);
    let neg_closure: Vec<_> = records
        .iter()
        .filter(|r| r.kind == DerivationKind::ClosureInferred && r.mult < 0)
        .map(|r| r.triple)
        .collect();
    assert!(
        neg_closure.contains(&(1, P, 4)),
        "expected a negative ClosureInferred for (1,P,4); got {neg_closure:?}"
    );

    // (c) Re-assert the broken edge — the closure re-derives the lost pairs.
    circuit.assert_triple((2, P, 3));
    circuit.tick();
    for &t in &[(1, P, 3), (1, P, 4), (2, P, 4)] {
        assert_eq!(
            circuit.derived_base().get(&t),
            1,
            "transitive edge {t:?} must be re-derived after re-assertion"
        );
    }
}

/// (b) Diamond: an edge supported by a second path is NOT withdrawn. Base
/// edges 1->2, 1->3, 2->4, 3->4 close (1,4) via two paths. Retracting (2,4)
/// must keep (1,4) [path 1->3->4 remains] but withdraw (2,4) itself.
#[test]
fn diamond_alternate_path_keeps_edge_alive() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((1, P, 3));
    circuit.assert_triple((2, P, 4));
    circuit.assert_triple((3, P, 4));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(1, P, 4)),
        1,
        "(1,P,4) closed via two paths"
    );

    let rx = circuit.subscribe();

    // Retract one arm of the diamond. (1,4) survives via 1->3->4; (2,4) was a
    // direct asserted edge being withdrawn, so its transitive contributions
    // that have no alternate path go too — but (1,4) is NOT one of them.
    circuit.retract_triple((2, P, 4));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(1, P, 4)),
        1,
        "(1,P,4) must persist — the 1->3->4 path still supports it"
    );

    // No negative ClosureInferred for (1,P,4) should have been published.
    let records = drain(&rx);
    let withdrew_1_4 = records
        .iter()
        .any(|r| r.triple == (1, P, 4) && r.kind == DerivationKind::ClosureInferred && r.mult < 0);
    assert!(
        !withdrew_1_4,
        "(1,P,4) must not be withdrawn while a second path supports it"
    );
}
