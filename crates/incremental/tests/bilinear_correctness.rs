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

use horndb_incremental::{BilinearRule, RuleId, TripleId, Zset};
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
