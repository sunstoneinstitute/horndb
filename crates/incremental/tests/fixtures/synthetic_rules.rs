//! Synthetic 3-rule OWL-2-RL-shaped ruleset used by the SPEC-06
//! acceptance #4 differential test.
//!
//! Predicate ID assignments (chosen arbitrarily, internal to this
//! fixture; SPEC-04 owns the real OWL 2 RL predicate IDs):
//!   SC   = 100  ("rdfs:subClassOf"-like)
//!   SPO  = 101  ("rdfs:subPropertyOf"-like)
//!   TYPE = 102  ("rdf:type"-like)

#![allow(dead_code)]

use reasoner_incremental::{BilinearRule, NaryPlan, RuleId, TripleId, Zset};

pub const SC: u64 = 100;
pub const SPO: u64 = 101;
pub const TYPE: u64 = 102;

pub const R1_SCM_SCO: RuleId = 1;
pub const R2_SCM_SPO: RuleId = 2;
pub const R3_CAX_SCO: RuleId = 3;

/// Bilinear self-join on a single predicate `p`: (?x p ?y) ∧ (?y p ?z) → (?x p ?z).
pub struct TransitiveOn {
    pub id: RuleId,
    pub p: u64,
}

impl BilinearRule for TransitiveOn {
    fn id(&self) -> RuleId {
        self.id
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, xp, xo), ma) in a.iter() {
            if *xp != self.p {
                continue;
            }
            for ((ys, yp, yo), mb) in b.iter() {
                if *yp != self.p {
                    continue;
                }
                if xo == ys {
                    out.add((*xs, self.p, *yo), ma * mb);
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

/// Bilinear cross-predicate join: (?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d).
pub struct CaxScoRule {
    pub id: RuleId,
}

impl BilinearRule for CaxScoRule {
    fn id(&self) -> RuleId {
        self.id
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
}

/// Build the three NaryPlans (each is a single bilinear) for the circuit.
pub fn build_plans() -> Vec<(NaryPlan, RuleId)> {
    let mut p1 = NaryPlan::new();
    p1.push_join(Box::new(TransitiveOn {
        id: R1_SCM_SCO,
        p: SC,
    }));
    let mut p2 = NaryPlan::new();
    p2.push_join(Box::new(TransitiveOn {
        id: R2_SCM_SPO,
        p: SPO,
    }));
    let mut p3 = NaryPlan::new();
    p3.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    vec![(p1, R1_SCM_SCO), (p2, R2_SCM_SPO), (p3, R3_CAX_SCO)]
}

/// Brute-force fixed-point reference. Repeatedly applies all three
/// rules to the asserted set ∪ derived set until no new triples
/// appear. Used as the gold standard for SPEC-06 acceptance #4.
///
/// Semantics: the closure is a *set* (each triple multiplicity = 1)
/// even though intermediate joins can produce arbitrary positive
/// multiplicities. After every round we normalise so that each
/// present key has multiplicity exactly 1; this matches the set
/// semantics the Circuit's semi-naïve "newly present" filter enforces.
pub fn full_rematerialize(asserted: &Zset<TripleId>) -> Zset<TripleId> {
    let r1 = TransitiveOn {
        id: R1_SCM_SCO,
        p: SC,
    };
    let r2 = TransitiveOn {
        id: R2_SCM_SPO,
        p: SPO,
    };
    let r3 = CaxScoRule { id: R3_CAX_SCO };
    let mut closure = asserted.clone();
    loop {
        let prev_len = closure.len();
        let d1 = r1.apply_full(&closure, &closure);
        let d2 = r2.apply_full(&closure, &closure);
        let d3 = r3.apply_full(&closure, &closure);
        // Add deltas only for keys not yet present, set-semantics.
        for (k, _m) in d1.iter().chain(d2.iter()).chain(d3.iter()) {
            if closure.get(k) == 0 {
                closure.add(*k, 1);
            }
        }
        if closure.len() == prev_len {
            break;
        }
    }
    closure
}
