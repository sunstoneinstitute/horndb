//! SPEC-06 F6 — closure-path retraction through the `Circuit`, and its
//! interaction with the rule recompute.
//!
//! On a retraction tick the closure plans run their retraction pass
//! (`apply_retract_delta`) BEFORE the rule recompute, withdrawing exactly the
//! `ClosureInferred` rows whose base support is gone and shrinking
//! `closure_support`. The rule recompute then seeds from
//! `asserted_base ∪ closure_support` — the already-shrunk support — so a rule
//! consequence that depended on a now-withdrawn closure edge is withdrawn with
//! it, and one that still has support survives.
//!
//! Closure withdrawal respects rule ownership (the dual of the original
//! Finding-2 logic): a row that is ALSO rule-derived keeps its materialization
//! from the rule; closure only loses its ownership. A row that loses BOTH rule
//! and closure support is withdrawn.

mod fixtures;

use fixtures::synthetic_rules::{CaxScoRule, TransitiveOn, R1_SCM_SCO, R3_CAX_SCO, SC, TYPE};
use horndb_incremental::{Circuit, NaryPlan, TransitiveClosureRule};

/// Closure-path retraction cascades into the rule consequence. Assert the SC
/// chain `(c,SC,d),(d,SC,e)` so the closure plan derives `(c,SC,e)`, plus
/// `(a,TYPE,c)` so the cax-sco rule derives `(a,TYPE,d)` (off asserted
/// `(c,SC,d)`) and `(a,TYPE,e)` (off the closure-derived `(c,SC,e)`).
///
/// Then retract the asserted SC edge `(d,SC,e)`. The remaining base SC edges
/// are `{(c,SC,d)}`, whose transitive closure is `{(c,SC,d)}` — so the closure
/// edge `(c,SC,e)` is no longer derivable and is **withdrawn**. The cax-sco
/// rule consequence `(a,TYPE,e)`, which had no support other than `(c,SC,e)`,
/// is therefore withdrawn with it. `(a,TYPE,d)` survives — `(c,SC,d)` remains.
#[test]
fn closure_consequence_withdrawn_when_support_retracted() {
    // Concrete distinct ids.
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;

    let mut circuit = Circuit::new();
    // SC closure plan — NOT a transitive *rule*, so the closure-derived
    // (c,SC,e) is reconstructible only by the closure plan.
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    circuit.add_plan(plan, R3_CAX_SCO);

    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((D, SC, E));
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "closure must derive (c,SC,e)"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, E)),
        1,
        "rule must derive (a,TYPE,e) off the closure edge"
    );

    // Retract the asserted SC edge (d,SC,e). Base SC after = {(c,SC,d)}, whose
    // closure = {(c,SC,d)}, so (c,SC,e) loses all support and is withdrawn; the
    // rule consequence (a,TYPE,e) goes with it.
    circuit.retract_triple((D, SC, E));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        0,
        "(c,SC,e) withdrawn — its base support (d,SC,e) is gone, no alternate path"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, E)),
        0,
        "(a,TYPE,e) withdrawn — its only support (closure edge (c,SC,e)) is gone"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        1,
        "(a,TYPE,d) survives — (c,SC,d) remains asserted"
    );
}

/// Overlap, both supports lost: the same triple `(1,SC,3)` is produced by BOTH
/// a transitive SC closure plan and a transitive SC rule. Retracting `(2,SC,3)`
/// removes the shared base edge: the rule no longer derives `(1,SC,3)` (base SC
/// after = `{(1,SC,2)}`), AND the closure plan withdraws it (its closure is
/// `{(1,SC,2)}`). With closure-path retraction live, the row loses BOTH supports
/// and must be withdrawn — the previous behavior (closure kept it alive) is gone.
#[test]
fn overlap_triple_withdrawn_when_shared_support_retracted() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(TransitiveOn {
        id: R1_SCM_SCO,
        p: SC,
    }));
    circuit.add_plan(plan, R1_SCM_SCO);

    circuit.assert_triple((1, SC, 2));
    circuit.assert_triple((2, SC, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, SC, 3)),
        1,
        "both rule and closure derive (1,SC,3)"
    );

    // Retract the shared base edge (2,SC,3). The closure withdraws (1,SC,3)
    // [closure of {(1,SC,2)} = {(1,SC,2)}] and the rule no longer derives it,
    // so the row loses all support.
    circuit.retract_triple((2, SC, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, SC, 3)),
        0,
        "(1,SC,3) withdrawn — both rule and closure support lost"
    );
}

/// Ghost case: a closure plan emits a *direct* edge `(c,SC,d)` that is ALSO
/// asserted by the user, so the edge is materialized only in `asserted_base`
/// (the closure pass's dedup-skip means it never lands in `derived_base`). A
/// rule `(a TYPE c) ∧ (c SC d) → (a TYPE d)` derives `(a,TYPE,d)` off that
/// asserted edge.
///
/// When the asserted `(c,SC,d)` is retracted, its only support disappears —
/// the closure plan does NOT independently materialize `(c,SC,d)` as a
/// non-asserted derived row — so the rule consequence `(a,TYPE,d)` MUST be
/// withdrawn.
///
/// Before the `closure_support ⊆ derived_base` fix, the closure pass recorded
/// `(c,SC,d)` in `closure_support` unconditionally (even though it lived only
/// in `asserted_base`). After retraction that ghost seeded
/// `recompute_rule_closure`, re-deriving `(a,TYPE,d)` from a triple the
/// materialized store says is gone. This test pins that the consequence is
/// now withdrawn.
#[test]
fn stale_rule_consequence_on_retracted_asserted_edge_is_withdrawn() {
    const A: u64 = 5;
    const C: u64 = 1;
    const D: u64 = 2;

    let mut circuit = Circuit::new();
    // SC closure plan: emits direct edges too, so an asserted (c,SC,d) is
    // re-emitted by the closure pass on its insertion tick.
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    circuit.add_plan(plan, R3_CAX_SCO);

    // Assert the SC edge (c,SC,d) and the type (a,TYPE,c). The rule derives
    // (a,TYPE,d) from the asserted SC edge.
    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        1,
        "rule derives (a,TYPE,d) from the asserted (c,SC,d)"
    );
    // (c,SC,d) is asserted, so the closure pass's dedup-skip keeps it out of
    // derived_base — it lives only in asserted_base.
    assert_eq!(
        circuit.derived_base().get(&(C, SC, D)),
        0,
        "(c,SC,d) is asserted, not materialized in derived_base"
    );

    // Retract the asserted (c,SC,d). Its only support is gone; the closure
    // plan does not independently materialize it as a derived row, so the
    // rule consequence (a,TYPE,d) must be withdrawn.
    circuit.retract_triple((C, SC, D));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, D)),
        0,
        "(c,SC,d) must not be a ghost materialized row after retraction"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        0,
        "(a,TYPE,d) must be withdrawn — its only support (asserted (c,SC,d)) is gone"
    );
}
