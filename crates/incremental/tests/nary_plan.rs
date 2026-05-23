//! F4: an n-ary rule is a left-deep tree of bilinear joins.
//!
//! We model a 3-pattern body (?x P ?y), (?y P ?z), (?z P ?w) inferring
//! (?x P ?w) as a tree of two prp-trp joins:
//!
//!   plan = Bilinear(P, P) → intermediate, then Bilinear(intermediate, P)
//!
//! and verify on a 4-node chain.

use reasoner_incremental::{BilinearRule, NaryPlan, RuleId, TripleId, Zset};

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
}

#[test]
fn left_deep_three_way_chain() {
    let r12 = PrpTrpOnP { id: 1 };
    let r23 = PrpTrpOnP { id: 2 };
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(r12));
    plan.push_join(Box::new(r23));

    // Base: 4-node chain 0-1-2-3 over P.
    let p_extent = Zset::from_iter([((0, P, 1), 1), ((1, P, 2), 1), ((2, P, 3), 1)]);

    // Full eval: should infer (0,P,2), (1,P,3), (0,P,3) and the
    // intermediate-pair derivations that compose to (0,P,3).
    let out = plan.apply_full(&p_extent);
    assert!(out.get(&(0, P, 3)) > 0, "transitive 3-hop must appear");
}
