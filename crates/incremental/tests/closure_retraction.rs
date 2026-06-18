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

/// BUG P2 — multiplicity-aware base deletion.
///
/// `assert_triple` appends `(triple, +1)` each call, so `asserted_base` is a
/// multiset: asserting `(1,P,2)` twice gives multiplicity 2. Retracting it
/// once leaves net multiplicity +1 — the base edge is STILL present — so the
/// closure backend must NOT delete it, and the downstream transitive edge
/// `(1,P,3)` must survive.
///
/// Before the fix, `Circuit::tick` forwarded every `m < 0` edge to
/// `delete_transitive_edges`, which set-removed the base edge regardless of
/// surviving multiplicity, prematurely withdrawing `(1,P,3)`.
#[test]
fn retract_one_of_two_asserted_copies_keeps_base_edge() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Assert (1,P,2) TWICE (multiplicity 2 in asserted_base) and (2,P,3) once.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();

    // Transitive (1,P,3) is closure-derived.
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(1,P,3) must be closure-derived from the chain"
    );
    assert_eq!(
        circuit.asserted_base().get(&(1, P, 2)),
        2,
        "(1,P,2) asserted twice => multiplicity 2"
    );

    let rx = circuit.subscribe();

    // Retract ONE copy of (1,P,2). Net asserted multiplicity is still +1, so the
    // base edge survives — (1,P,3) MUST NOT be withdrawn.
    circuit.retract_triple((1, P, 2));
    circuit.tick();

    assert_eq!(
        circuit.asserted_base().get(&(1, P, 2)),
        1,
        "(1,P,2) still has multiplicity 1 after retracting one of two copies"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(1,P,3) must SURVIVE — the base edge (1,P,2) is still present (mult 1)"
    );

    // No negative ClosureInferred for (1,P,3) should have been published.
    let records = drain(&rx);
    let withdrew_1_3 = records
        .iter()
        .any(|r| r.triple == (1, P, 3) && r.kind == DerivationKind::ClosureInferred && r.mult < 0);
    assert!(
        !withdrew_1_3,
        "(1,P,3) must not be withdrawn while the base edge survives"
    );
}

/// BUG P1 — promote an asserted-and-closure-derived triple on retraction.
///
/// Assert the chain `(1,P,2),(2,P,3)` AND the direct edge `(1,P,3)`. The
/// closure pass does NOT materialize `(1,P,3)` in `derived_base` (it is already
/// present as an asserted edge). Retracting the asserted `(1,P,3)` removes its
/// base edge, but `(1,3)` is STILL reachable via `(1,2),(2,3)` — so it must be
/// **promoted** to a materialized `ClosureInferred` derived row, and a POSITIVE
/// `ClosureInferred` must appear on the feed.
#[test]
fn retract_direct_edge_still_implied_promotes_to_derived() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Assert the chain AND the direct edge.
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.assert_triple((1, P, 3));
    circuit.tick();

    // (1,P,3) is asserted, so it lives in asserted_base, NOT derived_base.
    assert_eq!(
        circuit.asserted_base().get(&(1, P, 3)),
        1,
        "(1,P,3) asserted"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        0,
        "(1,P,3) is asserted, not materialized in derived_base"
    );

    let rx = circuit.subscribe();

    // Retract the direct (1,P,3). It is still implied by (1,2),(2,3), so it must
    // be promoted to a materialized ClosureInferred derived row.
    circuit.retract_triple((1, P, 3));
    circuit.tick();

    assert_eq!(
        circuit.asserted_base().get(&(1, P, 3)),
        0,
        "(1,P,3) no longer asserted after retraction"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(1,P,3) must be PROMOTED to a ClosureInferred derived row — still implied by (1,2),(2,3)"
    );
    // (1,P,2),(2,P,3) untouched (still asserted).
    assert_eq!(circuit.asserted_base().get(&(1, P, 2)), 1);
    assert_eq!(circuit.asserted_base().get(&(2, P, 3)), 1);

    // A POSITIVE ClosureInferred must appear for the promoted (1,P,3).
    let records = drain(&rx);
    let promoted = records
        .iter()
        .any(|r| r.triple == (1, P, 3) && r.kind == DerivationKind::ClosureInferred && r.mult > 0);
    assert!(
        promoted,
        "expected a POSITIVE ClosureInferred for the promoted (1,P,3); got {records:?}"
    );
}

/// BUG P1 dual — retracting the ALTERNATE path DOES withdraw `(1,P,3)` when it
/// is not separately asserted. Assert only the chain `(1,P,2),(2,P,3)` (so
/// `(1,P,3)` is purely closure-derived); retract `(2,P,3)` and the transitive
/// `(1,P,3)` must be withdrawn (no surviving path, nothing to promote).
#[test]
fn retract_alternate_path_withdraws_purely_derived_edge() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();
    assert_eq!(circuit.derived_base().get(&(1, P, 3)), 1);

    circuit.retract_triple((2, P, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        0,
        "(1,P,3) withdrawn — no surviving path, nothing to promote"
    );
}

/// Finding 4 — double-retract presence boundary.
///
/// An edge asserted ONCE (multiplicity 1) but retracted TWICE in a single tick
/// has post-tick multiplicity -1. The circuit treats any non-positive
/// multiplicity as absent, so the base edge is genuinely gone and the closure
/// deletion MUST fire. Before the fix the gate was `asserted_base.get(t) == 0`,
/// which a -1 multiplicity fails, suppressing the closure deletion and leaving
/// stale `ClosureInferred` rows.
#[test]
fn retract_twice_in_one_tick_withdraws_closure() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Assert the chain 1 -> 2 -> 3 (each edge once).
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(1,P,3) is closure-derived from the chain"
    );

    let rx = circuit.subscribe();

    // Retract (1,P,2) TWICE in the same tick: post-tick asserted multiplicity
    // is -1 (over-retracted), i.e. genuinely absent. The base edge is gone, so
    // the transitive (1,P,3) must be withdrawn.
    circuit.retract_triple((1, P, 2));
    circuit.retract_triple((1, P, 2));
    circuit.tick();

    assert!(
        circuit.asserted_base().get(&(1, P, 2)) <= 0,
        "(1,P,2) over-retracted: non-positive multiplicity => absent"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        0,
        "(1,P,3) must be withdrawn — the (1,P,2) base edge is gone"
    );

    // A negative ClosureInferred for (1,P,3) must appear on the feed.
    let records = drain(&rx);
    let withdrew = records
        .iter()
        .any(|r| r.triple == (1, P, 3) && r.kind == DerivationKind::ClosureInferred && r.mult < 0);
    assert!(
        withdrew,
        "expected a negative ClosureInferred for (1,P,3); got {records:?}"
    );
}

/// Finding 1 — the POSITIVE closure-insertion delta must respect POST-TICK
/// presence, not raw `m > 0` from the tick's asserted delta.
///
/// An edge over-retracted to a negative multiplicity and then partially
/// re-asserted so its NET post-tick multiplicity is `<= 0` is ABSENT. Feeding
/// the raw `m > 0` part of the tick's delta to the closure backend would derive
/// closure edges off a base edge that is not present.
///
/// Repro: assert `(1,P,2)` and `(2,P,3)`, tick → `(1,P,3)` derived. Retract
/// `(1,P,2)` TWICE in one tick → net asserted multiplicity -1, `(1,P,3)`
/// withdrawn. Then assert `(1,P,2)` ONCE → `asserted_base.get((1,P,2))` is now 0
/// (-1 + 1), still ABSENT — so the closure must NOT re-derive `(1,P,3)`.
#[test]
fn over_retract_then_reassert_to_zero_does_not_rederive_closure() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Assert the chain 1 -> 2 -> 3 (each edge once).
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((2, P, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(1,P,3) is closure-derived from the chain"
    );

    // Over-retract (1,P,2): two retractions in one tick → net multiplicity -1.
    circuit.retract_triple((1, P, 2));
    circuit.retract_triple((1, P, 2));
    circuit.tick();
    assert_eq!(
        circuit.asserted_base().get(&(1, P, 2)),
        -1,
        "(1,P,2) over-retracted to multiplicity -1"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        0,
        "(1,P,3) withdrawn after the base edge is gone"
    );

    let rx = circuit.subscribe();

    // Re-assert (1,P,2) ONCE: post-tick multiplicity is -1 + 1 = 0 → still
    // ABSENT. The closure must NOT re-derive (1,P,3) off an absent base edge.
    circuit.assert_triple((1, P, 2));
    circuit.tick();
    assert_eq!(
        circuit.asserted_base().get(&(1, P, 2)),
        0,
        "(1,P,2) re-asserted to net multiplicity 0 — still absent"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        0,
        "(1,P,3) must NOT be re-derived — the base edge (1,P,2) is absent (mult 0)"
    );

    // No POSITIVE ClosureInferred for (1,P,3) should have been published.
    let records = drain(&rx);
    let rederived = records
        .iter()
        .any(|r| r.triple == (1, P, 3) && r.kind == DerivationKind::ClosureInferred && r.mult > 0);
    assert!(
        !rederived,
        "(1,P,3) must not be re-derived off an absent base edge; got {records:?}"
    );
}

/// Finding 2 — DOCUMENT-AND-PIN (cosmetic change-feed precision, final state
/// already correct). A single MIXED tick that retracts one support path AND
/// inserts a replacement path for the SAME closure edge produces a transient
/// `ClosureInferred -1` then `+1` on the change feed (the deletion pass zeroes
/// the edge, then the insertion pass — running before the rule recompute, from
/// the round-2 fix — re-adds it), and `derived_merged` counts both. This is NOT
/// a final-state bug: `derived_base` is correct.
///
/// This test PINS the correct FINAL state. It deliberately does NOT assert on
/// the transient feed records (that net-delta reconciliation is a documented
/// Stage-2 follow-up — see `FUTURE-WORK.md`). A fresh subscriber that reads only
/// final state sees the right answer.
///
/// Setup: `c=1, d=2, e=3, x=4`, all over predicate `P`. The closure edge under
/// test is `(c,P,e)`. Path 1 is `c->d->e` (edges `(1,2),(2,3)`); the replacement
/// is `c->x->e` (edges `(1,4),(4,3)`). In one tick we retract `(d,P,e)` (breaking
/// path 1) and insert the two replacement edges. `(c,P,e)` must stay present.
#[test]
fn mixed_tick_replacement_path_final_state_correct() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    // Path 1: c -> d -> e closes the transitive (c,P,e).
    circuit.assert_triple((1, P, 2)); // c -> d
    circuit.assert_triple((2, P, 3)); // d -> e
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(c,P,e) closure-derived via c->d->e"
    );
    assert_eq!(
        circuit.derived_base().get(&(1, P, 4)),
        0,
        "no replacement path yet"
    );

    // MIXED tick: retract the d->e edge (breaking path 1) AND insert the
    // replacement path c->x->e in the same tick.
    circuit.retract_triple((2, P, 3)); // remove d -> e
    circuit.assert_triple((1, P, 4)); // c -> x
    circuit.assert_triple((4, P, 3)); // x -> e
    circuit.tick();

    // FINAL state must be correct:
    // (c,P,e) still present — now supported by c->x->e.
    assert_eq!(
        circuit.derived_base().get(&(1, P, 3)),
        1,
        "(c,P,e) must remain present — replacement path c->x->e supports it"
    );
    // The old path's derived/asserted contributions are gone:
    // d->e was asserted and is now retracted (absent from both bases).
    assert_eq!(
        circuit.asserted_base().get(&(2, P, 3)),
        0,
        "d->e retracted — gone from asserted_base"
    );
    assert_eq!(
        circuit.derived_base().get(&(2, P, 3)),
        0,
        "d->e has no materialized derived row"
    );
    // The replacement path's edges are present; (x,P,e) and (c,P,x) are asserted.
    assert_eq!(circuit.asserted_base().get(&(1, P, 4)), 1, "c->x asserted");
    assert_eq!(circuit.asserted_base().get(&(4, P, 3)), 1, "x->e asserted");

    // A fresh subscriber reading only final state via a snapshot sees (c,P,e).
    let snap = circuit.snapshot();
    assert!(
        snap.contains(&(1, P, 3)),
        "(c,P,e) must be visible in a fresh post-tick snapshot"
    );
}

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
