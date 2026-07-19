//! SPEC-24 S1 (PLAN-24-01) — targeted regression tests for delta-incremental
//! rule retraction.
//!
//! These pin the well-foundedness property of the two-phase (overdelete /
//! re-derive) fixpoint: a positive derivation weight alone must not keep a
//! row alive when its only support is cyclic (the flat weight-crossing rule
//! did exactly that — see the plan's AMENDED section). Task 3 extends this
//! file with the full transition matrix.

mod fixtures;

use std::collections::BTreeSet;

use fixtures::synthetic_rules::{build_plans, SC, TYPE};
use horndb_incremental::{ChangeFeedRx, Circuit, DeltaRecord, DerivationKind, TripleId};

fn circuit() -> Circuit {
    let mut c = Circuit::new();
    for (plan, rid) in build_plans() {
        c.add_plan(plan, rid);
    }
    c
}

/// Present derived rows (positive multiplicity), as a key set.
fn derived_keys(c: &Circuit) -> BTreeSet<TripleId> {
    c.derived_base()
        .iter()
        .filter(|(_, m)| *m > 0)
        .map(|(t, _)| *t)
        .collect()
}

/// Drain every buffered `RuleInferred` feed record from `rx`.
fn drain_rule_events(rx: &ChangeFeedRx) -> Vec<DeltaRecord> {
    let mut out = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        if matches!(rec.kind, DerivationKind::RuleInferred(_)) {
            out.push(rec);
        }
    }
    out
}

/// Assert the differential oracle's rule-closure key set equals the current
/// `derived_base` key set. PRECONDITION: no derived row is also asserted in
/// this scenario. The oracle seeds `asserted ∪ closure_support` and attributes
/// only *unseeded* derivable rows, so with rule-only fixtures its keys are the
/// rule-derivable rows not currently asserted; those coincide with the
/// (all-rule-materialized) `derived_base` keys exactly when no row is both
/// asserted and derived (otherwise divergence 1 applies — see PLAN-24-01).
fn assert_oracle_matches_derived(c: &Circuit) {
    let oracle: BTreeSet<TripleId> = c.oracle_rule_closure().into_keys().collect();
    assert_eq!(
        oracle,
        derived_keys(c),
        "oracle rule-closure keys must equal derived_base keys"
    );
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

/// Chain retraction cascade: SC chain a⊑b⊑c⊑d (ids 0..3). The closure derives
/// (0,SC,2), (1,SC,3), (0,SC,3). Retracting the middle edge (1,SC,2) leaves
/// {0→1, 2→3} with no composable pair, so ALL three derived rows are withdrawn
/// and the surviving closure is empty.
#[test]
fn chain_retraction_cascade_withdraws_dependent_closure() {
    let mut c = circuit();
    c.assert_triple((0, SC, 1));
    c.assert_triple((1, SC, 2));
    c.assert_triple((2, SC, 3));
    c.tick();
    c.debug_validate();
    assert_eq!(
        derived_keys(&c),
        BTreeSet::from([(0, SC, 2), (1, SC, 3), (0, SC, 3)]),
    );
    assert_oracle_matches_derived(&c);

    let rx = c.subscribe();
    c.retract_triple((1, SC, 2));
    c.tick();
    c.debug_validate();

    // Exact surviving closure: nothing.
    assert!(
        derived_keys(&c).is_empty(),
        "no closure row survives; derived = {:?}",
        derived_keys(&c)
    );
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 1);
    assert_eq!(c.asserted_base().get(&(2, SC, 3)), 1);
    assert_oracle_matches_derived(&c);

    // Exact withdrawn set: the three previously-derived rows, each -1.
    let withdrawn: BTreeSet<TripleId> = drain_rule_events(&rx)
        .into_iter()
        .inspect(|rec| assert_eq!(rec.mult, -1, "only withdrawals expected: {rec:?}"))
        .map(|rec| rec.triple)
        .collect();
    assert_eq!(
        withdrawn,
        BTreeSet::from([(0, SC, 2), (1, SC, 3), (0, SC, 3)]),
    );
}

/// Mutual cycle with external support (the partial-survivor case). Edges
/// a SC b, b SC a, c SC a (a=0,b=1,c=2). Closure derives (0,SC,0), (1,SC,1),
/// (2,SC,1). Retracting b SC a leaves {0→1, 2→0}: the self-loops (0,SC,0) and
/// (1,SC,1) die (cyclic-only support), but (2,SC,1) survives — it is
/// well-founded via c⊑a⊑b (2→0, 0→1). Because it was present before and stays
/// present, the retraction tick publishes NO feed transient for it.
#[test]
fn mutual_cycle_partial_survivor_no_transient() {
    let mut c = circuit();
    c.assert_triple((0, SC, 1));
    c.assert_triple((1, SC, 0));
    c.assert_triple((2, SC, 0));
    c.tick();
    c.debug_validate();
    assert_eq!(
        derived_keys(&c),
        BTreeSet::from([(0, SC, 0), (1, SC, 1), (2, SC, 1)]),
    );
    assert_oracle_matches_derived(&c);

    let rx = c.subscribe();
    c.retract_triple((1, SC, 0));
    c.tick();
    c.debug_validate();

    // Asserted survivors stay; the two self-loops die; (2,SC,1) survives.
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 1);
    assert_eq!(c.asserted_base().get(&(2, SC, 0)), 1);
    assert_eq!(c.asserted_base().get(&(1, SC, 0)), 0);
    assert_eq!(derived_keys(&c), BTreeSet::from([(2, SC, 1)]));
    assert_oracle_matches_derived(&c);

    // Net publishing: the two dead rows are withdrawn; no record whatsoever
    // for the well-founded survivor (2,SC,1).
    let events = drain_rule_events(&rx);
    let withdrawn: BTreeSet<TripleId> = events
        .iter()
        .filter(|rec| rec.mult < 0)
        .map(|rec| rec.triple)
        .collect();
    assert_eq!(withdrawn, BTreeSet::from([(0, SC, 0), (1, SC, 1)]));
    assert!(
        events.iter().all(|rec| rec.triple != (2, SC, 1)),
        "no feed transient for the survivor; saw {events:?}"
    );
}

/// Diamond re-derivation: (0,TYPE,3) is derivable two ways — via
/// (0,TYPE,1)∧(1,SC,3) and via (0,TYPE,2)∧(2,SC,3) — so its weight is 2.
/// Retracting one support drops the weight to 1; the row survives and the tick
/// publishes no feed record for it.
#[test]
fn diamond_rederivation_survives_one_support_loss() {
    let mut c = circuit();
    c.assert_triple((0, TYPE, 1));
    c.assert_triple((1, SC, 3));
    c.assert_triple((2, SC, 3));
    c.assert_triple((0, TYPE, 2));
    c.tick();
    c.debug_validate();
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, TYPE, 3)]));
    assert_oracle_matches_derived(&c);

    let rx = c.subscribe();
    c.retract_triple((0, TYPE, 1));
    c.tick();
    c.debug_validate();

    assert_eq!(c.derived_base().get(&(0, TYPE, 3)), 1, "row survives");
    assert!(
        drain_rule_events(&rx)
            .iter()
            .all(|rec| rec.triple != (0, TYPE, 3)),
        "no feed record for a still-derivable row"
    );
    assert_oracle_matches_derived(&c);
}

/// Re-assert round-trip: retract a base edge then re-assert it across ticks;
/// the store returns to its original state, and the feed shows exactly the
/// withdraw then the re-add of the dependent derived row.
#[test]
fn reassert_round_trip_restores_store() {
    let mut c = circuit();
    c.assert_triple((0, SC, 1));
    c.assert_triple((1, SC, 2));
    c.tick();
    c.debug_validate();
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, SC, 2)]));

    let rx = c.subscribe();
    c.retract_triple((1, SC, 2));
    c.tick();
    c.debug_validate();
    assert!(derived_keys(&c).is_empty());

    c.assert_triple((1, SC, 2));
    c.tick();
    c.debug_validate();

    // Store equals the original.
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 1);
    assert_eq!(c.asserted_base().get(&(1, SC, 2)), 1);
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, SC, 2)]));

    // Feed shows the withdraw (-1) then the re-add (+1) of (0,SC,2).
    let events = drain_rule_events(&rx);
    assert_eq!(events.len(), 2, "one withdraw + one re-add; saw {events:?}");
    assert_eq!(events[0].triple, (0, SC, 2));
    assert_eq!(events[0].mult, -1);
    assert_eq!(events[1].triple, (0, SC, 2));
    assert_eq!(events[1].mult, 1);
}

/// Mixed single tick: retract one support AND insert an equivalent replacement
/// in ONE tick. (0,TYPE,3) is derived via (0,TYPE,1)∧(1,SC,3); the tick swaps
/// its support to (0,TYPE,2)∧(2,SC,3). The row is unaffected on net, so no rule
/// feed event is published for it, and the final store is correct.
#[test]
fn mixed_single_tick_support_swap_no_transient() {
    let mut c = circuit();
    c.assert_triple((0, TYPE, 1));
    c.assert_triple((1, SC, 3));
    c.assert_triple((2, SC, 3));
    c.tick();
    c.debug_validate();
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, TYPE, 3)]));

    let rx = c.subscribe();
    // One tick: drop the old support, add the replacement support.
    c.retract_triple((0, TYPE, 1));
    c.assert_triple((0, TYPE, 2));
    c.tick();
    c.debug_validate();

    // Final store: support switched, consequence retained.
    assert_eq!(c.asserted_base().get(&(0, TYPE, 1)), 0);
    assert_eq!(c.asserted_base().get(&(0, TYPE, 2)), 1);
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, TYPE, 3)]));

    // No net rule feed event for the unaffected consequence.
    assert!(
        drain_rule_events(&rx)
            .iter()
            .all(|rec| rec.triple != (0, TYPE, 3)),
        "unaffected row must not churn the feed"
    );
}

/// Duplicate asserts / over-retraction: multiplicity accounting at the
/// presence boundary. Assert (0,SC,1) twice; one retraction leaves it present
/// (2→1), the second withdraws it (1→0) and cascades the consequence away; an
/// over-retraction of a never-asserted triple is a no-op that keeps the
/// invariants intact.
#[test]
fn duplicate_asserts_and_over_retraction() {
    let mut c = circuit();
    c.assert_triple((0, SC, 1));
    c.assert_triple((0, SC, 1)); // multiplicity 2
    c.assert_triple((1, SC, 2));
    c.tick();
    c.debug_validate();
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 2);
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, SC, 2)]));

    // First retraction: 2 → 1, still present; consequence survives.
    c.retract_triple((0, SC, 1));
    c.tick();
    c.debug_validate();
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 1);
    assert_eq!(derived_keys(&c), BTreeSet::from([(0, SC, 2)]));

    // Second retraction: 1 → 0, gone; consequence withdrawn.
    c.retract_triple((0, SC, 1));
    c.tick();
    c.debug_validate();
    assert_eq!(c.asserted_base().get(&(0, SC, 1)), 0);
    assert!(derived_keys(&c).is_empty());

    // Over-retraction of a never-asserted triple: no effect, invariants hold.
    c.retract_triple((9, SC, 9));
    c.tick();
    c.debug_validate();
    assert!(derived_keys(&c).is_empty());
    assert!(c.asserted_base().get(&(9, SC, 9)) <= 0);
}
