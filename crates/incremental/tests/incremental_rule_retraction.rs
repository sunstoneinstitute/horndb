//! SPEC-24 S1 (PLAN-24-01) — targeted regression tests for delta-incremental
//! rule retraction.
//!
//! These pin the well-foundedness property of the two-phase (overdelete /
//! re-derive) fixpoint: a positive derivation weight alone must not keep a
//! row alive when its only support is cyclic (the flat weight-crossing rule
//! did exactly that — see the plan's AMENDED section). Task 3 extends this
//! file with the full transition matrix.

mod fixtures;

use fixtures::synthetic_rules::{build_plans, SC, TYPE};
use horndb_incremental::{Circuit, DerivationKind};

fn circuit() -> Circuit {
    let mut c = Circuit::new();
    for (plan, rid) in build_plans() {
        c.add_plan(plan, rid);
    }
    c
}

/// Direct self-support: with `3 SC 3` asserted, cax-sco derives `(5,TYPE,3)`
/// from itself (`x TYPE c ∧ c SC c → x TYPE c`), so its weight is positive
/// while it sits in the extent. Retracting the asserted copy must kill the
/// row — its only rule support is cyclic.
#[test]
fn self_supported_row_dies_when_asserted_copy_retracted() {
    let mut c = circuit();
    c.assert_triple((5, TYPE, 3));
    c.tick();
    c.assert_triple((3, SC, 3));
    c.tick();
    c.retract_triple((5, TYPE, 3));
    c.tick();

    assert_eq!(c.asserted_base().get(&(3, SC, 3)), 1);
    assert_eq!(c.asserted_base().get(&(5, TYPE, 3)), 0);
    assert!(
        c.derived_base().is_empty(),
        "self-supported row must not survive; derived = {:?}",
        c.derived_base().iter().collect::<Vec<_>>()
    );
}

/// Mutual SC cycle: `0 SC 1` and `1 SC 0` derive `(0,SC,0)` and `(1,SC,1)`
/// (each also re-derivable through the cycle). Retracting one cycle edge
/// must collapse the whole derived web — every remaining derivation routes
/// through the retracted edge or through cyclic self-support.
#[test]
fn mutual_sc_cycle_collapses_on_edge_retraction() {
    let mut c = circuit();
    c.assert_triple((0, SC, 1));
    c.assert_triple((1, SC, 0));
    c.tick();
    assert_eq!(c.derived_base().get(&(0, SC, 0)), 1);
    assert_eq!(c.derived_base().get(&(1, SC, 1)), 1);

    c.retract_triple((0, SC, 1));
    c.tick();

    assert_eq!(c.asserted_base().get(&(1, SC, 0)), 1);
    assert!(
        c.derived_base().is_empty(),
        "cyclic web must collapse; derived = {:?}",
        c.derived_base().iter().collect::<Vec<_>>()
    );
}

/// Retracting the asserted copy of a row that rules still WELL-FOUNDEDLY
/// derive re-materializes it as rule-owned, publishing exactly one
/// `RuleInferred +1` for it (net publishing — no transient), matching the
/// Stage-1 recompute outcome.
#[test]
fn retracted_asserted_copy_with_well_founded_support_promotes_to_rule_row() {
    let mut c = circuit();
    c.assert_triple((7, TYPE, 1));
    c.assert_triple((1, SC, 2));
    c.assert_triple((7, TYPE, 2)); // also derivable via cax-sco
    c.tick();
    // Asserted-covered: weights recorded, but no derived row.
    assert_eq!(c.derived_base().get(&(7, TYPE, 2)), 0);

    let rx = c.subscribe();
    c.retract_triple((7, TYPE, 2));
    c.tick();

    assert_eq!(c.asserted_base().get(&(7, TYPE, 2)), 0);
    assert_eq!(
        c.derived_base().get(&(7, TYPE, 2)),
        1,
        "row survives as rule-materialized"
    );
    let mut rule_events = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        if matches!(rec.kind, DerivationKind::RuleInferred(_)) {
            rule_events.push(rec);
        }
    }
    assert_eq!(
        rule_events.len(),
        1,
        "exactly one net rule event; saw {rule_events:?}"
    );
    assert_eq!(rule_events[0].triple, (7, TYPE, 2));
    assert_eq!(rule_events[0].mult, 1);
}
