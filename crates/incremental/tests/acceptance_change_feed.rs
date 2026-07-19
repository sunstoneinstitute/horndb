//! SPEC-06 acceptance #5: change-feed correctness.
//!
//! Property: under any sequence of insertions ticked through the
//! Circuit, a subscriber sees every committed (asserted + derived)
//! delta exactly once, in publication order, with no gaps and no
//! duplicates.

mod fixtures;

use fixtures::synthetic_rules::{build_plans, SC, SPO, TYPE};
use horndb_incremental::{Circuit, DerivationKind};
use std::collections::HashSet;

#[test]
fn no_gaps_no_duplicates_under_sustained_inserts() {
    let mut circuit = Circuit::new();
    for (plan, rid) in build_plans() {
        circuit.add_plan(plan, rid);
    }
    let rx = circuit.subscribe();

    // 1000 insertions across 100 ticks (10 per tick), drawn from a
    // 5×5×5 ID space so we get lots of join opportunities.
    let mut asserted_count = 0;
    for tick_i in 0..100u64 {
        for j in 0..10u64 {
            let s = (tick_i * 10 + j) % 5;
            let p = match (tick_i + j) % 3 {
                0 => SC,
                1 => SPO,
                _ => TYPE,
            };
            let o = (tick_i + j) % 5;
            circuit.assert_triple((s, p, o));
            asserted_count += 1;
        }
        circuit.tick();
    }

    // Drain the feed.
    let mut all = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        all.push(rec);
    }

    // Asserted records: exactly one per assert_triple call. Some may
    // be "noop" (insert of already-present triple, which still
    // publishes — the tick's drain phase publishes every log append).
    let asserted: Vec<_> = all
        .iter()
        .filter(|r| matches!(r.kind, DerivationKind::Asserted))
        .collect();
    assert_eq!(
        asserted.len(),
        asserted_count,
        "every assert_triple must produce exactly one Asserted feed record"
    );

    // Asserted logical times are unique and strictly monotonic.
    let asserted_times: Vec<u64> = asserted.iter().map(|r| r.time).collect();
    let unique: HashSet<_> = asserted_times.iter().collect();
    assert_eq!(
        unique.len(),
        asserted_times.len(),
        "duplicate asserted times"
    );
    for w in asserted_times.windows(2) {
        assert!(w[0] < w[1], "asserted times must be strictly increasing");
    }

    // Derived records: every (triple, mult, rule_id) corresponds to
    // a row currently in derived_base (no spurious publishes).
    for rec in all.iter() {
        if let DerivationKind::RuleInferred(_) = rec.kind {
            // Either the row is present in derived_base, or a later
            // retraction publish (mult = -1) cancelled it. This suite
            // is insertion-only, so the second case cannot occur;
            // assert presence.
            assert!(
                circuit.derived_base().get(&rec.triple) > 0,
                "derived feed record {:?} has no matching base row",
                rec.triple
            );
        }
    }
}
