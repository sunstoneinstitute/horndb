//! SPEC-06 F5 — closure-operator deltas through the Circuit.

use horndb_incremental::{Circuit, DerivationKind, TransitiveClosureRule};

const P: u64 = 100;

/// Inserting a chain across two ticks yields the transitive edge, emitted
/// once as a ClosureInferred derived triple.
#[test]
fn chain_closure_across_ticks() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));

    c.assert_triple((1, P, 2));
    c.tick();
    c.assert_triple((2, P, 3));
    c.tick();

    assert_eq!(c.asserted_base().get(&(1, P, 2)), 1);
    assert_eq!(c.asserted_base().get(&(2, P, 3)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 3)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 2)), 0);
    assert_eq!(c.derived_base().get(&(2, P, 3)), 0);
}

/// A single tick that inserts a full chain closes it in one pass.
#[test]
fn chain_closure_one_tick() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));
    c.assert_triple((1, P, 2));
    c.assert_triple((2, P, 3));
    c.assert_triple((3, P, 4));
    c.tick();
    assert_eq!(c.derived_base().get(&(1, P, 3)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 4)), 1);
    assert_eq!(c.derived_base().get(&(2, P, 4)), 1);
    assert_eq!(c.derived_base().get(&(1, P, 2)), 0);
    assert_eq!(c.derived_base().get(&(2, P, 3)), 0);
    assert_eq!(c.derived_base().get(&(3, P, 4)), 0);
}

/// The change feed receives each closure-inferred triple once, tagged
/// ClosureInferred (F9), with no duplicates.
#[test]
fn change_feed_tags_closure_inferred() {
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(P)));
    let rx = c.subscribe();

    c.assert_triple((1, P, 2));
    c.assert_triple((2, P, 3));
    c.tick();

    let mut asserted = Vec::new();
    let mut closure = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        match rec.kind {
            DerivationKind::Asserted => asserted.push(rec.triple),
            DerivationKind::ClosureInferred => closure.push(rec.triple),
            DerivationKind::RuleInferred(_) => panic!("no rule plans registered"),
        }
    }
    asserted.sort_unstable();
    closure.sort_unstable();
    assert_eq!(asserted, vec![(1, P, 2), (2, P, 3)]);
    assert_eq!(closure, vec![(1, P, 3)]);
}
