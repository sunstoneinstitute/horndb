//! Verify the F3 decomposition law for a reference bilinear rule.
//!
//! Rule: `prp-trp` style transitivity over a single fixed predicate P.
//! Body: `(?x P ?y) ∧ (?y P ?z)` → head: `(?x P ?z)`.
//!
//! We assert `Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B` over arbitrary
//! Z-sets of triples on the predicate P. `A` and `B` are both views of
//! the same relation (the predicate's extent) in `prp-trp`; we keep
//! them separate in the trait because most bilinear rules are joins of
//! two distinct patterns.

use horndb_incremental::{BilinearRule, HashJoinRule, RuleId, TripleId, Zset};
use horndb_wcoj::{Term, TriplePattern, Var};
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
        // Naïve nested-loop join for the reference implementation.
        // SPEC-04 codegen will emit hash/sort-merge variants; here we
        // only need correctness, not speed.
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != P {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != P {
                    continue;
                }
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

fn arb_p_triples(n: usize) -> impl Strategy<Value = Zset<TripleId>> {
    prop::collection::vec((0u64..6, 0u64..6), 0..n)
        .prop_map(|edges| Zset::from_iter(edges.into_iter().map(|(s, o)| ((s, P, o), 1))))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn bilinear_decomposition_matches_full_recompute(
        a in arb_p_triples(10),
        da in arb_p_triples(4),
        b in arb_p_triples(10),
        db in arb_p_triples(4),
    ) {
        let rule = PrpTrpOnP { id: 1 };

        // Reference: full recompute on (A + ΔA) ⋈ (B + ΔB) minus A ⋈ B.
        let mut a_full = a.clone(); a_full.add_assign(&da);
        let mut b_full = b.clone(); b_full.add_assign(&db);
        let mut reference = rule.apply_full(&a_full, &b_full);
        let base = rule.apply_full(&a, &b);
        reference.sub_assign(&base);

        let decomposed = rule.apply_delta(&a, &b, &da, &db);

        prop_assert_eq!(reference, decomposed);
    }
}

// --- HashJoinRule vs. nested-loop reference (SPEC-24 §S7 leaf 2) -----------
//
// `kernels::HashJoinRule` must derive exactly what a hand-written
// nested-loop `BilinearRule` derives, for any Z-set multiplicities — not
// just the sets `arb_p_triples` above generates for the decomposition-law
// check. Three body shapes, each with its own nested-loop reference kept
// independent of the kernel under test (unlike `tests/fixtures`, which now
// builds its rules on `HashJoinRule` itself and so can't serve as an
// independent oracle here):
//
// (a) self-join on one predicate:      (?x P ?y) ∧ (?y P ?z)  → (?x P ?z)
// (b) cross-predicate join:            (?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d)
// (c) `Bound` object + head constant:  (?x P ?y) ∧ (?y Q c) → (?x R ?y)

const TYPE: u64 = 20;
const SC: u64 = 21;
const Q: u64 = 22;
const R_PRED: u64 = 23;
const C_OBJ: u64 = 2;

/// Reference for shape (b), a nested-loop copy independent of
/// `tests/fixtures::synthetic_rules::CaxScoRule` (which now delegates to
/// `HashJoinRule` and so can't serve as this test's oracle).
struct CaxScoRef;
impl BilinearRule for CaxScoRef {
    fn id(&self) -> RuleId {
        2
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != TYPE {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != SC {
                    continue;
                }
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

/// Reference for shape (c): a `Bound` object on the right pattern
/// (`(?y Q c)`) and a head predicate that is neither body predicate.
struct BoundObjectRef;
impl BilinearRule for BoundObjectRef {
    fn id(&self) -> RuleId {
        3
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != P {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != Q || *yo != C_OBJ {
                    continue;
                }
                if xo == ys {
                    out.add((*xs, R_PRED, *xo), ma * mb);
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
        [Some(P), Some(Q)]
    }
}

fn self_join_kernel() -> HashJoinRule {
    let (x, y, z) = (Term::Var(Var(0)), Term::Var(Var(1)), Term::Var(Var(2)));
    let p = Term::Bound(P);
    HashJoinRule::new(
        1,
        TriplePattern::new(x, p, y),
        TriplePattern::new(y, p, z),
        TriplePattern::new(x, p, z),
    )
    .unwrap()
}

fn cax_sco_kernel() -> HashJoinRule {
    let (x, c, d) = (Term::Var(Var(0)), Term::Var(Var(1)), Term::Var(Var(2)));
    HashJoinRule::new(
        2,
        TriplePattern::new(x, Term::Bound(TYPE), c),
        TriplePattern::new(c, Term::Bound(SC), d),
        TriplePattern::new(x, Term::Bound(TYPE), d),
    )
    .unwrap()
}

fn bound_object_kernel() -> HashJoinRule {
    let (x, y) = (Term::Var(Var(0)), Term::Var(Var(1)));
    HashJoinRule::new(
        3,
        TriplePattern::new(x, Term::Bound(P), y),
        TriplePattern::new(y, Term::Bound(Q), Term::Bound(C_OBJ)),
        TriplePattern::new(x, Term::Bound(R_PRED), y),
    )
    .unwrap()
}

/// Arbitrary Z-set of triples `(s, pred, o)` with `s`/`o` drawn from a small
/// id space (so joins actually fire — including on the `Bound` object `2` in
/// shape (c)) and multiplicities in `-3..=3` (duplicate keys sum via
/// `Zset::from_iter`, dropping to absent on 0).
fn arb_zset_full(pred: u64, n: usize) -> impl Strategy<Value = Zset<TripleId>> {
    prop::collection::vec((0u64..6, 0u64..6, -3i64..=3), 0..n)
        .prop_map(move |rows| Zset::from_iter(rows.into_iter().map(|(s, o, m)| ((s, pred, o), m))))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Shape (a): self-join on P.
    #[test]
    fn hash_join_matches_nested_loop_self_join(
        a in arb_zset_full(P, 10),
        da in arb_zset_full(P, 4),
        b in arb_zset_full(P, 10),
        db in arb_zset_full(P, 4),
    ) {
        let reference = PrpTrpOnP { id: 1 };
        let kernel = self_join_kernel();
        prop_assert_eq!(reference.apply_full(&a, &b), kernel.apply_full(&a, &b));
        prop_assert_eq!(
            reference.apply_delta(&a, &b, &da, &db),
            kernel.apply_delta(&a, &b, &da, &db)
        );
    }

    /// Shape (b): cross-predicate join TYPE/SC.
    #[test]
    fn hash_join_matches_nested_loop_cross_predicate(
        a in arb_zset_full(TYPE, 10),
        da in arb_zset_full(TYPE, 4),
        b in arb_zset_full(SC, 10),
        db in arb_zset_full(SC, 4),
    ) {
        let reference = CaxScoRef;
        let kernel = cax_sco_kernel();
        prop_assert_eq!(reference.apply_full(&a, &b), kernel.apply_full(&a, &b));
        prop_assert_eq!(
            reference.apply_delta(&a, &b, &da, &db),
            kernel.apply_delta(&a, &b, &da, &db)
        );
    }

    /// Shape (c): `Bound` object on the right pattern, head predicate
    /// distinct from either body predicate.
    #[test]
    fn hash_join_matches_nested_loop_bound_object(
        a in arb_zset_full(P, 10),
        da in arb_zset_full(P, 4),
        b in arb_zset_full(Q, 10),
        db in arb_zset_full(Q, 4),
    ) {
        let reference = BoundObjectRef;
        let kernel = bound_object_kernel();
        prop_assert_eq!(reference.apply_full(&a, &b), kernel.apply_full(&a, &b));
        prop_assert_eq!(
            reference.apply_delta(&a, &b, &da, &db),
            kernel.apply_delta(&a, &b, &da, &db)
        );
    }
}
