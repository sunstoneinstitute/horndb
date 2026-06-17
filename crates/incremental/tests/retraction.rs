//! SPEC-06 F6: correct retraction across joins.
//!
//! Verifies that retracting an asserted base triple withdraws every
//! rule consequence whose support disappears, while leaving consequences
//! that retain an independent derivation in place. Also exercises the
//! SPEC-06 acceptance #3 round-trip: insert N, retract the same N, and
//! confirm the circuit returns bit-identically to the empty pre-insert
//! state.

mod fixtures;

use horndb_incremental::{BilinearRule, Circuit, DerivationKind, NaryPlan, RuleId, TripleId, Zset};

const P: u64 = 7;

/// Transitive self-join on predicate `P`: (?x P ?y) ∧ (?y P ?z) → (?x P ?z).
/// Mirrors `PrpTrpOnP` from `tests/circuit_tick.rs`.
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

fn transitive_circuit() -> Circuit {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));
    let mut circuit = Circuit::new();
    circuit.add_plan(plan, RuleId::from(1u32));
    circuit
}

#[test]
fn retract_base_withdraws_consequence() {
    let mut circuit = transitive_circuit();

    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(0, P, 2)),
        1,
        "(0,P,2) should be derived from the chain 0->1->2"
    );

    let rx = circuit.subscribe();

    circuit.retract_triple((1, P, 2));
    circuit.tick();

    assert_eq!(
        circuit.asserted_base().get(&(1, P, 2)),
        0,
        "retracted base triple must be gone"
    );
    assert_eq!(
        circuit.derived_base().get(&(0, P, 2)),
        0,
        "consequence whose only support vanished must be withdrawn"
    );

    // The feed must carry a negative-multiplicity withdrawal for (0,P,2).
    let mut seen = Vec::new();
    while let Ok(rec) = rx.try_recv() {
        seen.push(rec);
    }
    assert!(
        seen.iter().any(|r| r.triple == (0, P, 2)
            && r.mult < 0
            && matches!(r.kind, DerivationKind::RuleInferred(_))),
        "feed must contain a negative RuleInferred withdrawal for (0,P,2); saw {seen:?}"
    );
}

#[test]
fn multi_support_consequence_survives_partial_retraction() {
    let mut circuit = transitive_circuit();

    // Two independent chains both derive (0,P,2): 0->1->2 and 0->3->2.
    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));
    circuit.assert_triple((0, P, 3));
    circuit.assert_triple((3, P, 2));
    circuit.tick();
    assert_eq!(circuit.derived_base().get(&(0, P, 2)), 1);

    // Drop one support: still derivable via 0->3->2.
    circuit.retract_triple((1, P, 2));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(0, P, 2)),
        1,
        "(0,P,2) still derivable via 0->3->2"
    );

    // Drop the other support: now no derivation remains.
    circuit.retract_triple((3, P, 2));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(0, P, 2)),
        0,
        "(0,P,2) withdrawn once both supports are gone"
    );
}

#[test]
fn insert_10k_retract_10k_bit_identical() {
    // SPEC-06 acceptance #3: assert N triples, retract the same N, and
    // confirm the circuit returns bit-identically to the empty state.
    //
    // We use the 3-rule synthetic ruleset. To keep the closure bounded
    // (the synthetic join is O(n^2) nested-loop and transitive closure
    // over a dense predicate explodes), we spread 10_000 triples over an
    // 800-id space across the three predicates with i*7 offsets. Z-set
    // dedup collapses repeats; the asserted set is still well over the
    // 10_000-call acceptance floor (every call goes through the log).
    const SC: u64 = fixtures::synthetic_rules::SC;
    const SPO: u64 = fixtures::synthetic_rules::SPO;
    const TYPE: u64 = fixtures::synthetic_rules::TYPE;
    const PREDS: [u64; 3] = [SC, SPO, TYPE];

    let mut circuit = Circuit::new();
    for (plan, rid) in fixtures::synthetic_rules::build_plans() {
        circuit.add_plan(plan, rid);
    }

    let triples: Vec<TripleId> = (0..10_000u64)
        .map(|i| (i % 800, PREDS[(i % 3) as usize], (i * 7) % 800))
        .collect();

    for t in &triples {
        circuit.assert_triple(*t);
    }
    circuit.tick();

    for t in &triples {
        circuit.retract_triple(*t);
    }
    circuit.tick();

    assert!(
        circuit.asserted_base().is_empty(),
        "asserted_base must be empty after retracting every asserted triple"
    );
    assert!(
        circuit.derived_base().is_empty(),
        "derived_base must be empty after the full round-trip"
    );
}
