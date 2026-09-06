//! F4: an n-ary rule is a left-deep tree of bilinear joins.
//!
//! We model a 3-pattern body (?x P ?y), (?y P ?z), (?z P ?w) inferring
//! (?x P ?w) as a tree of two prp-trp joins:
//!
//!   plan = Bilinear(P, P) → intermediate, then Bilinear(intermediate, P)
//!
//! and verify on a 4-node chain.

mod fixtures;

use fixtures::synthetic_rules::{CaxScoRule, SC, SPO, TYPE};
use horndb_incremental::{BilinearRule, NaryPlan, PredExtent, RuleId, TripleId, Zset};
use proptest::prelude::*;

const P: u64 = 7;

struct PrpTrpOnP {
    id: RuleId,
}
impl BilinearRule for PrpTrpOnP {
    fn id(&self) -> RuleId {
        self.id
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys {
                    out.add((*xs, P, *yo), ma * mb);
                }
            }
        }
        out
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
    fn body_predicates(&self) -> [Option<u64>; 2] {
        [Some(P), Some(P)]
    }
}

#[test]
fn left_deep_three_way_chain() {
    let r12 = PrpTrpOnP { id: 1 };
    let r23 = PrpTrpOnP { id: 2 };
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(r12));
    plan.push_join(Box::new(r23));

    // Base: 4-node chain 0-1-2-3 over P.
    let p_extent = PredExtent::from_zset(&Zset::from_iter([
        ((0, P, 1), 1),
        ((1, P, 2), 1),
        ((2, P, 3), 1),
    ]));

    // Full eval: should infer (0,P,2), (1,P,3), (0,P,3) and the
    // intermediate-pair derivations that compose to (0,P,3).
    let out = plan.apply_full(&p_extent);
    assert!(out.get(&(0, P, 3)) > 0, "transitive 3-hop must appear");
}

/// A `CaxSco`-shaped join (`(?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d)`) that,
/// unlike `fixtures::synthetic_rules::CaxScoRule`, trusts its inputs and
/// applies no internal predicate filter — it joins purely on `xo == ys`.
/// This is the shape SPEC-04 codegen emits once leaves are sliced upstream
/// by `NaryPlan` (SPEC-24 S7): a leaf no longer needs to defend against
/// off-predicate rows because `NaryPlan` never hands it any.
struct CaxScoTrusting {
    id: RuleId,
}
impl BilinearRule for CaxScoTrusting {
    fn id(&self) -> RuleId {
        self.id
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys {
                    out.add((*xs, TYPE, *yo), ma * mb);
                }
            }
        }
        out
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
    fn body_predicates(&self) -> [Option<u64>; 2] {
        [Some(TYPE), Some(SC)]
    }
}

/// DoD: a trusting (unfiltered) leaf declaring `body_predicates` must derive
/// exactly what the reference `CaxScoRule` (which filters internally)
/// derives, even when the base extent also holds unrelated `SPO` rows that
/// would spuriously join if a leaf read the whole extent instead of its
/// declared predicate slice.
///
/// This must fail on whole-extent leaves: `TYPE`-shaped and `SC`-shaped rows
/// alone can't produce a spurious cross-join, so the fixture also seeds
/// `SPO` rows whose subjects/objects collide with `TYPE`/`SC` rows on
/// purpose, so an unsliced leaf pulls in extra `SPO` rows on both sides and
/// derives spurious `(?, TYPE, ?)` triples the reference never does.
#[test]
fn trusting_join_matches_reference_over_a_mixed_predicate_extent() {
    let extent = PredExtent::from_zset(&Zset::from_iter([
        // TYPE and SC rows the reference CaxScoRule is meant to join.
        ((1, TYPE, 20), 1),
        ((2, TYPE, 21), 1),
        ((20, SC, 30), 1),
        ((21, SC, 31), 1),
        // SPO rows sharing subjects/objects with the rows above. A leaf
        // that reads the whole extent (ignoring its declared predicate)
        // picks these up too, producing spurious TYPE derivations that a
        // predicate-sliced leaf must not.
        ((1, SPO, 11), 1),
        ((10, SPO, 20), 1),
        ((10, SPO, 21), 1),
    ]));

    let mut reference_plan = NaryPlan::new();
    reference_plan.push_join(Box::new(CaxScoRule { id: 1 }));
    let expected = reference_plan.apply_full(&extent);

    let mut trusting_plan = NaryPlan::new();
    trusting_plan.push_join(Box::new(CaxScoTrusting { id: 1 }));
    let actual = trusting_plan.apply_full(&extent);

    assert_eq!(actual, expected);
}

/// Builds a fresh two-join left-deep plan (`PrpTrpOnP` chained twice), the
/// shape the stateful-eval tests below drive.
fn two_join_plan() -> NaryPlan {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));
    plan.push_join(Box::new(PrpTrpOnP { id: 2 }));
    plan
}

/// Builds a fresh three-join left-deep plan (`PrpTrpOnP` chained three
/// times), the shape `left_deep_three_way_chain` uses. Exercises the
/// `state[1..]` level of `apply_delta_stateful` that `two_join_plan` (only
/// `state[0]`) never reaches, and the cold-start inner fold in
/// `apply_delta_stateful` that only iterates for `joins.len() > 2`.
fn three_join_plan() -> NaryPlan {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));
    plan.push_join(Box::new(PrpTrpOnP { id: 2 }));
    plan.push_join(Box::new(PrpTrpOnP { id: 3 }));
    plan
}

#[test]
fn stateful_cold_start_matches_full() {
    let base = Zset::from_iter([((0, P, 1), 1), ((1, P, 2), 1), ((2, P, 3), 1)]);
    let delta = Zset::from_iter([((3, P, 4), 1)]);

    let mut base_plus_delta = base.clone();
    base_plus_delta.add_assign(&delta);

    let reference_plan = two_join_plan();
    let mut expected = reference_plan.apply_full(&PredExtent::from_zset(&base_plus_delta));
    expected.sub_assign(&reference_plan.apply_full(&PredExtent::from_zset(&base)));

    let mut stateful_plan = two_join_plan();
    let actual = stateful_plan.apply_delta_stateful(&PredExtent::from_zset(&base), &delta);

    assert_eq!(actual, expected);
}

#[test]
fn reset_state_reinitializes() {
    let mut plan = two_join_plan();

    // Drive a couple of stateful rounds over one base.
    let mut running_base = Zset::from_iter([((0, P, 1), 1), ((1, P, 2), 1)]);
    let delta1 = Zset::from_iter([((2, P, 3), 1)]);
    let _ = plan.apply_delta_stateful(&PredExtent::from_zset(&running_base), &delta1);
    running_base.add_assign(&delta1);

    let delta2 = Zset::from_iter([((0, P, 1), -1)]);
    let _ = plan.apply_delta_stateful(&PredExtent::from_zset(&running_base), &delta2);
    running_base.add_assign(&delta2);

    plan.reset_state();

    // Continue against a completely different base after the reset.
    let different_base = Zset::from_iter([((10, P, 11), 1), ((11, P, 12), 1)]);
    let delta3 = Zset::from_iter([((12, P, 13), 1)]);

    let expected = two_join_plan().apply_delta(&PredExtent::from_zset(&different_base), &delta3);
    let actual = plan.apply_delta_stateful(&PredExtent::from_zset(&different_base), &delta3);

    assert_eq!(actual, expected);
}

/// Strategy for a triple in a small id space (predicate fixed — `PrpTrpOnP`
/// matches its inputs regardless of predicate, only the id space matters
/// for join fan-out).
fn small_triple() -> impl Strategy<Value = TripleId> {
    (0u64..6, Just(P), 0u64..6).prop_map(|(s, p, o)| (s, p, o))
}

/// A batch of candidate presence flips: `bool` is "wants to be present".
fn small_batch() -> impl Strategy<Value = Vec<(TripleId, bool)>> {
    prop::collection::vec((small_triple(), any::<bool>()), 1..5)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Drives 1-20 random set-semantics delta batches through a two-join
    /// plan. Compares the stateful evaluator (fed only the running base as
    /// the pre-round extent) against a freshly-built stateless plan's
    /// `apply_delta` over the same (base, batch) pair at every step.
    #[test]
    fn stateful_delta_matches_stateless_over_random_sequences(
        batches in prop::collection::vec(small_batch(), 1..20)
    ) {
        let mut running_base: Zset<TripleId> = Zset::new();
        let mut stateful_plan = two_join_plan();

        for batch in batches {
            // Convert candidate flips into a set-semantics delta: insert
            // only keys absent from the (running_base + delta-so-far)
            // view, retract only keys present in it.
            let mut delta: Zset<TripleId> = Zset::new();
            for (t, want_present) in batch {
                let currently = running_base.get(&t) + delta.get(&t) > 0;
                if want_present && !currently {
                    delta.add(t, 1);
                } else if !want_present && currently {
                    delta.add(t, -1);
                }
            }

            let base_ext = PredExtent::from_zset(&running_base);
            let expected = two_join_plan().apply_delta(&base_ext, &delta);
            let actual = stateful_plan.apply_delta_stateful(&base_ext, &delta);
            prop_assert_eq!(actual, expected);

            running_base.add_assign(&delta);
        }
    }

    /// Same drill as `stateful_delta_matches_stateless_over_random_sequences`
    /// but over `three_join_plan()`, so the level loop in
    /// `apply_delta_stateful` runs for both `state[0]` and `state[1]` and
    /// its cold-start inner fold actually iterates.
    #[test]
    fn stateful_three_join_matches_stateless_over_random_sequences(
        batches in prop::collection::vec(small_batch(), 1..20)
    ) {
        let mut running_base: Zset<TripleId> = Zset::new();
        let mut stateful_plan = three_join_plan();

        for batch in batches {
            let mut delta: Zset<TripleId> = Zset::new();
            for (t, want_present) in batch {
                let currently = running_base.get(&t) + delta.get(&t) > 0;
                if want_present && !currently {
                    delta.add(t, 1);
                } else if !want_present && currently {
                    delta.add(t, -1);
                }
            }

            let base_ext = PredExtent::from_zset(&running_base);
            let expected = three_join_plan().apply_delta(&base_ext, &delta);
            let actual = stateful_plan.apply_delta_stateful(&base_ext, &delta);
            prop_assert_eq!(actual, expected);

            running_base.add_assign(&delta);
        }
    }
}
