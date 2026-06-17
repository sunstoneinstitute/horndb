//! SPEC-06 F6 — interaction of retraction recompute with closure-derived
//! inputs (codex review findings).
//!
//! Finding 1: a rule consequence that depends on a closure-derived input
//! must survive a retraction tick that removes the rule's *asserted* path
//! to that consequence while the closure support stays intact. The
//! retraction recompute seeds its rule closure from
//! `asserted_base ∪ closure_support`, so a closure-inferred row (e.g.
//! `(c,SC,e)`) is a stable base input the recompute can join against rather
//! than something it omits and spuriously withdraws.
//!
//! Finding 2: when the same triple is produced by both a rule and a
//! closure plan, losing the rule support must NOT delete the row —
//! closure-path retraction is deferred, so the closure support keeps it
//! alive. The retraction diff retains any withdrawn rule row that is in
//! `closure_support`.

mod fixtures;

use fixtures::synthetic_rules::{CaxScoRule, TransitiveOn, R1_SCM_SCO, R3_CAX_SCO, SC, TYPE};
use horndb_incremental::{Circuit, NaryPlan, TransitiveClosureRule};

/// Finding 1: assert the SC chain `(c,SC,d),(d,SC,e)` so the (non-SC-
/// transitive) closure plan derives `(c,SC,e)`, plus `(a,TYPE,c)` so the
/// cax-sco rule derives `(a,TYPE,d)` and (via the closure edge / the
/// asserted `(d,SC,e)`) `(a,TYPE,e)`.
///
/// Then retract the asserted SC edge `(d,SC,e)`. The rule's *asserted*
/// route to `(a,TYPE,e)` is gone, but the insertion-only closure plan
/// still supports `(c,SC,e)`, so the rule must still derive `(a,TYPE,e)`
/// off the closure-derived edge. Before the Finding-1 fix the recompute
/// seeds only `asserted_base`, omits `(c,SC,e)`, and spuriously withdraws
/// `(a,TYPE,e)`.
#[test]
fn rule_consequence_on_closure_input_survives_unrelated_retraction() {
    // Concrete distinct ids.
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;

    let mut circuit = Circuit::new();
    // SC closure plan — NOT a transitive *rule*, so the closure-derived
    // (c,SC,e) is not reconstructible by the registered rules.
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
        "rule must derive (a,TYPE,e)"
    );

    // Retract the asserted SC edge (d,SC,e). The rule's asserted route to
    // (a,TYPE,e) disappears, but the closure plan (insertion-only) still
    // supports (c,SC,e), so the rule can still derive (a,TYPE,e).
    circuit.retract_triple((D, SC, E));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "(c,SC,e) persists — closure-path retraction is deferred"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, E)),
        1,
        "(a,TYPE,e) must NOT be spuriously withdrawn — it is still derivable \
         from the closure-derived (c,SC,e)"
    );
}

/// Finding 2: the same triple `(1,SC,3)` is produced by BOTH a transitive
/// SC closure plan and a transitive SC rule. Retracting one of its rule
/// supports loses the rule derivation, but the insertion-only closure plan
/// still supports it, so the row must be retained.
#[test]
fn overlap_triple_retains_closure_support_on_rule_retraction() {
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

    // Retract a rule support. The rule no longer derives (1,SC,3), but the
    // insertion-only closure plan still supports it.
    circuit.retract_triple((2, SC, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, SC, 3)),
        1,
        "(1,SC,3) retained via closure_support after rule support lost"
    );
}
