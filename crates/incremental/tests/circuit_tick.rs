//! Integration: insert a triple, tick the circuit, observe the
//! derived consequence in the base store AND on the change feed.
//!
//! Reuses the prp-trp-shape rule from the bilinear-correctness test
//! but routed via a Circuit so we exercise the wiring end-to-end.

use horndb_incremental::{
    BilinearRule, Circuit, DerivationKind, NaryPlan, RuleId, TripleId, Zset,
};

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
fn insert_two_edges_then_tick_derives_transitive_consequence() {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));

    let mut circuit = Circuit::new();
    circuit.add_plan(plan, RuleId::from(1u32));
    let rx = circuit.subscribe();

    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));

    let report = circuit.tick();
    assert_eq!(report.asserted_merged, 2);
    assert!(report.derived_merged >= 1, "should derive at least (0,P,2)");

    // Base store contains the derivation.
    assert_eq!(circuit.derived_base().get(&(0, P, 2)), 1);

    // Change feed contains 2 asserted + ≥1 derived record. Drain.
    let mut seen = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        seen.push(rec);
    }
    assert!(seen
        .iter()
        .any(|r| r.triple == (0, P, 1) && r.kind == DerivationKind::Asserted));
    assert!(seen
        .iter()
        .any(|r| r.triple == (0, P, 2) && matches!(r.kind, DerivationKind::RuleInferred(1))));
}
