//! SPEC-06 acceptance #4: incremental ≡ full re-materialization.
//!
//! Now that F6 (correct retraction across joins) has landed, this
//! differential test covers **both insertion and retraction** and
//! checks **multiplicity equality**, not just support-set membership.
//!
//! Each proptest drives a sequence of operations through a single
//! `Circuit`, ticking in between, then asserts that the Circuit's
//! materialized store — the union (asserted ∪ derived) — matches the
//! fixed-point reference `full_rematerialize(net_asserted)` exactly,
//! including the multiplicity of every present triple (set semantics:
//! every present triple appears exactly once, multiplicity 1).
//!
//! - `insert_then_tick_*` and `tick_per_insert_*` exercise the
//!   forward-only path with coarse- and fine-grained ticking.
//! - `interleaved_insert_retract_*` interleaves insertions and
//!   retractions per-op so retractions race with derivations.
//! - `coarse_mixed_ticks_*`, `incremental_matches_recompute_fallback`
//!   and `unguarded_ops_keep_invariants` are the SPEC-24 S1 gate
//!   (PLAN-24-01 Task 4): coarse mixed batches per tick, an A/B run
//!   against the recompute-fallback twin, and fully unguarded input
//!   (duplicate asserts, over-retractions) — each tick followed by
//!   `debug_validate()`.

mod fixtures;

use std::collections::BTreeSet;

use fixtures::synthetic_rules::{build_plans, full_rematerialize, SC, SPO, TYPE};
use horndb_incremental::{Circuit, TripleId, Zset};
use proptest::prelude::*;

/// Asserts that the Circuit's materialized store equals the reference,
/// at full **multiplicity equality** (not just support-set membership).
///
/// The Circuit parks a triple in either `asserted_base` or
/// `derived_base`; the materialized store is their union `U`. The
/// reference is `R = full_rematerialize(net_asserted)`, which is a
/// clean set (every present key at multiplicity 1). After F6 the
/// Circuit must maintain the same clean set semantics, so we verify:
///
/// 1. `U` and `R` have the same key set (no missing, no spurious).
/// 2. `derived_base` carries no zero or negative rows: every derived
///    multiplicity is exactly 1 (the Zset invariant forbids zeros; this
///    additionally rules out negative leftovers from retraction and any
///    derivation-count drift in the inferred store). This is the real
///    content of "multiplicity equality": after F6 a derived triple is
///    present exactly once regardless of how many supports it has.
/// 3. The union presence is exactly 1 per key (set semantics — a triple
///    that is both asserted and derived still appears once). The
///    reference is already all-1, so matching it pins the store to a
///    clean set.
///
/// Note: `asserted_base` itself may hold a multiplicity > 1 when the
/// caller asserts the same triple more than once — that is user input
/// multiplicity, not derivation drift, and it does not change the union
/// presence (still 1). We therefore do not constrain the raw
/// asserted-input count here; the invariants that matter live on the
/// union and on `derived_base`.
///
/// Returns true on equivalence; on failure, prints the offending triple
/// and its multiplicity and returns false.
///
/// Caveat: the union construction below does not filter negative rows, so
/// `asserted` must not carry over-retraction leftovers (m < 0). Callers
/// with unguarded ops must project through `positive_presence` first.
fn check_equivalence(asserted: &Zset<TripleId>, derived: &Zset<TripleId>) -> bool {
    let reference = full_rematerialize(asserted);

    // derived_base must be clean: every present row at multiplicity 1
    // (no zeros — Zset invariant — and crucially no negatives).
    for (k, m) in derived.iter() {
        if m != 1 {
            eprintln!("derived_base has non-unit multiplicity: {k:?} -> {m}");
            return false;
        }
    }

    // Build the union U = asserted ∪ derived as a *presence* set: a key
    // present on either side contributes exactly one row. asserted_base
    // may carry a multiplicity > 1 when the same triple is asserted more
    // than once (input multiplicity is the user's, not derivation drift);
    // the materialized store still treats the key as present exactly
    // once, so the union presence is 1. The multiplicity invariant we
    // hold the engine to is on the union and on derived_base, not on the
    // raw asserted-input count.
    let mut union: Zset<TripleId> = Zset::new();
    for (k, _m) in asserted.iter() {
        if union.get(k) == 0 {
            union.add(*k, 1);
        }
    }
    for (k, _m) in derived.iter() {
        if union.get(k) == 0 {
            union.add(*k, 1);
        }
    }

    // Same key set, and every present union key at multiplicity exactly 1.
    for (k, m) in union.iter() {
        if reference.get(k) == 0 {
            eprintln!("spurious: {k:?} (mult {m})");
            return false;
        }
        if m != 1 {
            eprintln!("union key {k:?} has multiplicity {m}, expected 1");
            return false;
        }
    }
    for (k, m) in reference.iter() {
        if union.get(k) == 0 {
            eprintln!("missing: {k:?} (reference mult {m})");
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

/// A sequence of insert/retract operations over the small ID/predicate
/// space the forward-only tests use. `bool` is `is_retract`.
fn small_random_ops() -> impl Strategy<Value = Vec<(TripleId, bool)>> {
    let pred = prop::sample::select(vec![SC, SPO, TYPE]);
    let triple = (0u64..6, pred, 0u64..6).prop_map(|(s, p, o)| (s, p, o));
    let op = (triple, any::<bool>());
    prop::collection::vec(op, 1..30)
}

/// Coarse-tick shape for the SPEC-24 S1 gate tests: 1–6 ticks, each fed
/// a batch of 1–8 insert/retract ops (`bool` is `is_retract`).
fn batched_random_ops() -> impl Strategy<Value = Vec<Vec<(TripleId, bool)>>> {
    let pred = prop::sample::select(vec![SC, SPO, TYPE]);
    let triple = (0u64..6, pred, 0u64..6).prop_map(|(s, p, o)| (s, p, o));
    let op = (triple, any::<bool>());
    prop::collection::vec(prop::collection::vec(op, 1..=8), 1..=6)
}

/// The union `asserted ∪ derived` as a *presence* key set (rows with
/// positive multiplicity on either side). This is the A/B comparison
/// surface: the incremental and fallback paths may park the same present
/// triple on different sides (PLAN-24-01 "Expected behavioral
/// divergences"), so raw `derived_base` key sets are not comparable —
/// union presence is.
fn union_presence(asserted: &Zset<TripleId>, derived: &Zset<TripleId>) -> BTreeSet<TripleId> {
    asserted
        .iter()
        .filter(|(_, m)| *m > 0)
        .map(|(t, _)| *t)
        .chain(derived.iter().filter(|(_, m)| *m > 0).map(|(t, _)| *t))
        .collect()
}

/// Positive-presence projection of a Z-set: every key with positive
/// multiplicity, at multiplicity exactly 1. Used to turn a raw
/// `asserted_base` that may hold duplicate-assert multiplicities (> 1)
/// or over-retraction leftovers (< 0) into the presence set the store
/// semantics are defined over.
fn positive_presence(z: &Zset<TripleId>) -> Zset<TripleId> {
    Zset::from_iter(z.iter().filter(|(_, m)| *m > 0).map(|(t, _)| (*t, 1)))
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

    /// Interleave insertions and retractions, ticking after every op so
    /// retractions race with derivations. A parallel "net asserted"
    /// model tracks the well-formed asserted set; we only ever retract a
    /// triple that is currently net-present, mirroring the same guard in
    /// both the circuit and the model so `asserted_base` stays in {0,1}
    /// presence and never goes negative.
    #[test]
    fn interleaved_insert_retract_matches_full_rematerialize(ops in small_random_ops()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }

        // Model of the net-asserted set. Presence == get(&t) > 0; the
        // guard below keeps every present key at exactly multiplicity 1.
        let mut net: Zset<TripleId> = Zset::new();

        for (triple, is_retract) in &ops {
            if *is_retract {
                // Only retract a currently net-present triple — keeps
                // both the circuit's asserted_base and the model in
                // lockstep, never driving asserted_base negative.
                if net.get(triple) > 0 {
                    net.add(*triple, -1);
                    circuit.retract_triple(*triple);
                    circuit.tick();
                }
                // else: skip — nothing to withdraw.
            } else if net.get(triple) == 0 {
                // Insert only if not already present (set semantics):
                // asserting a duplicate would push asserted_base to 2.
                net.add(*triple, 1);
                circuit.assert_triple(*triple);
                circuit.tick();
            }
            // else: already present, skip duplicate insert.
        }

        // The model is already a clean presence set (every key at mult 1
        // by construction), so it doubles as the net-asserted presence.
        prop_assert!(
            check_equivalence(circuit.asserted_base(), circuit.derived_base()),
            "incremental store diverges from full re-materialization after interleaved insert/retract"
        );

        // Sanity: the circuit's asserted_base must equal the model.
        prop_assert_eq!(
            circuit.asserted_base().len(),
            net.len(),
            "circuit asserted_base diverged from net model"
        );
        for (k, _m) in net.iter() {
            prop_assert_eq!(
                circuit.asserted_base().get(k),
                1,
                "model triple absent from circuit asserted_base: {:?}",
                k
            );
        }
    }

    /// SPEC-24 S1 gate (a): coarse mixed ticks. Each tick sees a whole
    /// batch of 1–8 ops (insert or presence-guarded retract) at once, so
    /// a single tick mixes insertions and retractions — the case the
    /// unified tick's two-phase (overdelete, re-derive) scheme must get
    /// right. After every tick: `debug_validate()` (incremental-state
    /// invariants) and `check_equivalence` (store ≡ full re-materialize).
    #[test]
    fn coarse_mixed_ticks_match_full_rematerialize(batches in batched_random_ops()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }

        // Net-asserted model; same presence guard as the interleaved
        // test, but applied per-op *within* a batch so the batch itself
        // stays well-formed (no duplicate insert, no over-retract).
        let mut net: Zset<TripleId> = Zset::new();

        for batch in &batches {
            for (triple, is_retract) in batch {
                if *is_retract {
                    if net.get(triple) > 0 {
                        net.add(*triple, -1);
                        circuit.retract_triple(*triple);
                    }
                } else if net.get(triple) == 0 {
                    net.add(*triple, 1);
                    circuit.assert_triple(*triple);
                }
            }
            // One coarse tick per batch. A batch whose ops were all
            // guarded away yields an empty tick — that must be safe too.
            circuit.tick();
            circuit.debug_validate();
            prop_assert!(
                check_equivalence(circuit.asserted_base(), circuit.derived_base()),
                "incremental store diverges from full re-materialization after a coarse mixed tick"
            );
        }
    }

    /// SPEC-24 S1 gate (b): A/B against the recompute-fallback twin. The
    /// identical presence-guarded op sequence drives a `Circuit::new()`
    /// and a `Circuit::new_with_recompute_fallback()`; after every tick
    /// both must agree on union presence and hold every derived row at
    /// multiplicity exactly 1. Raw `derived_base` key sets may legally
    /// differ (PLAN-24-01 "Expected behavioral divergences": the two
    /// paths can park a both-asserted-and-derivable triple on different
    /// sides), so the comparison is union presence, never derived keys.
    #[test]
    fn incremental_matches_recompute_fallback(batches in batched_random_ops()) {
        let mut inc = Circuit::new();
        let mut fb = Circuit::new_with_recompute_fallback();
        for (plan, rid) in build_plans() {
            inc.add_plan(plan, rid);
        }
        for (plan, rid) in build_plans() {
            fb.add_plan(plan, rid);
        }

        let mut net: Zset<TripleId> = Zset::new();

        for batch in &batches {
            for (triple, is_retract) in batch {
                if *is_retract {
                    if net.get(triple) > 0 {
                        net.add(*triple, -1);
                        inc.retract_triple(*triple);
                        fb.retract_triple(*triple);
                    }
                } else if net.get(triple) == 0 {
                    net.add(*triple, 1);
                    inc.assert_triple(*triple);
                    fb.assert_triple(*triple);
                }
            }
            inc.tick();
            fb.tick();
            inc.debug_validate();
            // The fallback resyncs its incremental state every retraction
            // tick (`resync_incremental_state`), so its invariants must
            // hold too.
            fb.debug_validate();

            for (k, m) in inc.derived_base().iter() {
                prop_assert_eq!(m, 1, "incremental derived_base {:?} at multiplicity {}", k, m);
            }
            for (k, m) in fb.derived_base().iter() {
                prop_assert_eq!(m, 1, "fallback derived_base {:?} at multiplicity {}", k, m);
            }

            let inc_presence = union_presence(inc.asserted_base(), inc.derived_base());
            let fb_presence = union_presence(fb.asserted_base(), fb.derived_base());
            prop_assert_eq!(
                inc_presence,
                fb_presence,
                "incremental and recompute-fallback circuits diverge on union presence"
            );
        }
    }

    /// SPEC-24 S1 gate (c): fully unguarded ops — duplicate asserts and
    /// over-retractions allowed — with coarse ticks. `asserted_base` may
    /// then hold any multiplicity, including negative (over-retraction is
    /// a no-op at the presence level but leaves a negative Z-set row), so
    /// the reference is built from the *positive-presence projection* of
    /// `asserted_base`. After every tick the invariants must still hold
    /// (`debug_validate()`) and the union presence must equal
    /// `full_rematerialize(presence(asserted))`.
    #[test]
    fn unguarded_ops_keep_invariants(batches in batched_random_ops()) {
        let mut circuit = Circuit::new();
        for (plan, rid) in build_plans() {
            circuit.add_plan(plan, rid);
        }

        for batch in &batches {
            for (triple, is_retract) in batch {
                if *is_retract {
                    circuit.retract_triple(*triple);
                } else {
                    circuit.assert_triple(*triple);
                }
            }
            circuit.tick();
            circuit.debug_validate();

            // check_equivalence must see a presence set on the asserted
            // side: the raw asserted_base can hold negative rows here,
            // which its union construction would wrongly count as
            // present. Its reference is then exactly
            // full_rematerialize(presence(asserted)).
            let asserted_presence = positive_presence(circuit.asserted_base());
            prop_assert!(
                check_equivalence(&asserted_presence, circuit.derived_base()),
                "store diverges from full re-materialization of the asserted presence \
                 projection under unguarded ops"
            );
        }
    }
}
