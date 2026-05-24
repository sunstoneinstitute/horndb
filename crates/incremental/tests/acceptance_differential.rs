//! SPEC-06 acceptance #4: incremental ≡ full re-materialization.
//!
//! Stage 1 scope: insertions only. We pick a sequence of insertions,
//! drive them through a Circuit one batch at a time with tick() in
//! between, then assert the Circuit's derived_base equals the
//! fixed-point reference run from scratch on the cumulative asserted
//! set (minus the asserted set itself, which lives in asserted_base).
//!
//! We tick after every individual insert *and* after every batch of
//! inserts, to exercise both fine-grained and coarse-grained
//! incrementalisation.

mod fixtures;

use fixtures::synthetic_rules::{build_plans, full_rematerialize, SC, SPO, TYPE};
use proptest::prelude::*;
use horndb_incremental::{Circuit, TripleId, Zset};

/// Returns true if `incremental` equals the reference run.
///
/// Compared as **set membership over the union (asserted ∪ derived)**.
/// The Circuit may park a triple in either `asserted_base` or
/// `derived_base` depending on the order of inserts and derivations:
/// a triple first derived from a rule and *later* asserted will live in
/// both bases. DBSP set semantics says the closure is the union; the
/// reference computes the union directly. We therefore compare the
/// support sets, ignoring multiplicities (Stage 1 is set-semantics).
fn check_equivalence(asserted: &Zset<TripleId>, derived: &Zset<TripleId>) -> bool {
    let reference = full_rematerialize(asserted);
    let mut union = asserted.clone();
    for (k, _m) in derived.iter() {
        if union.get(k) == 0 {
            union.add(*k, 1);
        }
    }

    for (k, _) in reference.iter() {
        if union.get(k) == 0 {
            eprintln!("missing: {k:?}");
            return false;
        }
    }
    for (k, _) in union.iter() {
        if reference.get(k) == 0 {
            eprintln!("spurious: {k:?}");
            return false;
        }
    }
    true
}

fn small_random_inserts() -> impl Strategy<Value = Vec<TripleId>> {
    let pred = prop::sample::select(vec![SC, SPO, TYPE]);
    let triple = (0u64..6, pred, 0u64..6).prop_map(|(s, p, o)| (s, p, o));
    prop::collection::vec(triple, 1..20)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    #[test]
    fn insert_then_tick_matches_full_rematerialize(inserts in small_random_inserts()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }

        for triple in &inserts {
            circuit.assert_triple(*triple);
        }
        // One coarse tick.
        circuit.tick();

        prop_assert!(
            check_equivalence(circuit.asserted_base(), circuit.derived_base()),
            "incremental derived set diverges from full re-materialization reference"
        );
    }

    #[test]
    fn tick_per_insert_matches_full_rematerialize(inserts in small_random_inserts()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }
        for triple in &inserts {
            circuit.assert_triple(*triple);
            circuit.tick();
        }
        prop_assert!(
            check_equivalence(circuit.asserted_base(), circuit.derived_base())
        );
    }
}
