//! Synthetic 3-rule OWL-2-RL-shaped ruleset used by the SPEC-06
//! acceptance #4 differential test.
//!
//! Predicate ID assignments (chosen arbitrarily, internal to this
//! fixture; SPEC-04 owns the real OWL 2 RL predicate IDs):
//!   SC   = 100  ("rdfs:subClassOf"-like)
//!   SPO  = 101  ("rdfs:subPropertyOf"-like)
//!   TYPE = 102  ("rdf:type"-like)

#![allow(dead_code)]

use horndb_incremental::{BilinearRule, HashJoinRule, NaryPlan, RuleId, TripleId, Zset};
use horndb_wcoj::{Term, TriplePattern, Var};

pub const SC: u64 = 100;
pub const SPO: u64 = 101;
pub const TYPE: u64 = 102;

pub const R1_SCM_SCO: RuleId = 1;
pub const R2_SCM_SPO: RuleId = 2;
pub const R3_CAX_SCO: RuleId = 3;

/// Bilinear self-join on a single predicate `p`: (?x p ?y) ∧ (?y p ?z) → (?x p ?z).
/// A thin `HashJoinRule` wrapper — kept as its own named struct (rather than
/// a bare `HashJoinRule` value) because other fixtures/tests construct it by
/// struct literal (`TransitiveOn { id, p }`).
pub struct TransitiveOn {
    pub id: RuleId,
    pub p: u64,
}

impl TransitiveOn {
    fn kernel(&self) -> HashJoinRule {
        let x = Term::Var(Var(0));
        let y = Term::Var(Var(1));
        let z = Term::Var(Var(2));
        let p = Term::Bound(self.p);
        HashJoinRule::new(
            self.id,
            TriplePattern::new(x, p, y),
            TriplePattern::new(y, p, z),
            TriplePattern::new(x, p, z),
        )
        .expect("TransitiveOn's shape is a valid HashJoinRule")
    }
}

impl BilinearRule for TransitiveOn {
    fn id(&self) -> RuleId {
        self.id
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        self.kernel().apply_full(a, b)
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        self.kernel().apply_delta(a, b, da, db)
    }
    fn body_predicates(&self) -> [Option<u64>; 2] {
        self.kernel().body_predicates()
    }
}

/// Bilinear cross-predicate join: (?x TYPE ?c) ∧ (?c SC ?d) → (?x TYPE ?d).
pub struct CaxScoRule {
    pub id: RuleId,
}

impl CaxScoRule {
    fn kernel(&self) -> HashJoinRule {
        let x = Term::Var(Var(0));
        let c = Term::Var(Var(1));
        let d = Term::Var(Var(2));
        HashJoinRule::new(
            self.id,
            TriplePattern::new(x, Term::Bound(TYPE), c),
            TriplePattern::new(c, Term::Bound(SC), d),
            TriplePattern::new(x, Term::Bound(TYPE), d),
        )
        .expect("CaxScoRule's shape is a valid HashJoinRule")
    }
}

impl BilinearRule for CaxScoRule {
    fn id(&self) -> RuleId {
        self.id
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        self.kernel().apply_full(a, b)
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        self.kernel().apply_delta(a, b, da, db)
    }
    fn body_predicates(&self) -> [Option<u64>; 2] {
        self.kernel().body_predicates()
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
