//! SPEC-06 F6 — closure-path retraction through the `Circuit`, and its
//! interaction with the rule fixpoint.
//!
//! On a retraction tick the closure plans run their retraction pass
//! (`apply_retract_delta`) BEFORE the rule fixpoint, withdrawing exactly the
//! `ClosureInferred` rows whose base support is gone and shrinking
//! `closure_support`. The unified tick (PLAN-24-01) feeds those presence
//! changes into the incremental R1/R2 rule fixpoint — so a rule consequence
//! that depended on a now-withdrawn closure edge is withdrawn with it, and
//! one that still has support survives.
//!
//! Closure withdrawal respects rule ownership (the dual of the original
//! Finding-2 logic): a row that is ALSO rule-derived keeps its materialization
//! from the rule; closure only loses its ownership. A row that loses BOTH rule
//! and closure support is withdrawn.

mod fixtures;

use fixtures::synthetic_rules::{
    CaxScoRule, TransitiveOn, R1_SCM_SCO, R2_SCM_SPO, R3_CAX_SCO, SC, TYPE,
};
use horndb_incremental::{Circuit, NaryPlan, TransitiveClosureRule};

/// Closure-path retraction cascades into the rule consequence. Assert the SC
/// chain `(c,SC,d),(d,SC,e)` so the closure plan derives `(c,SC,e)`, plus
/// `(a,TYPE,c)` so the cax-sco rule derives `(a,TYPE,d)` (off asserted
/// `(c,SC,d)`) and `(a,TYPE,e)` (off the closure-derived `(c,SC,e)`).
///
/// Then retract the asserted SC edge `(d,SC,e)`. The remaining base SC edges
/// are `{(c,SC,d)}`, whose transitive closure is `{(c,SC,d)}` — so the closure
/// edge `(c,SC,e)` is no longer derivable and is **withdrawn**. The cax-sco
/// rule consequence `(a,TYPE,e)`, which had no support other than `(c,SC,e)`,
/// is therefore withdrawn with it. `(a,TYPE,d)` survives — `(c,SC,d)` remains.
#[test]
fn closure_consequence_withdrawn_when_support_retracted() {
    // Concrete distinct ids.
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;

    let mut circuit = Circuit::new();
    // SC closure plan — NOT a transitive *rule*, so the closure-derived
    // (c,SC,e) is reconstructible only by the closure plan.
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    circuit.add_plan(plan, R3_CAX_SCO);

    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((D, SC, E));
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "closure must derive (c,SC,e)"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, E)),
        1,
        "rule must derive (a,TYPE,e) off the closure edge"
    );

    // Retract the asserted SC edge (d,SC,e). Base SC after = {(c,SC,d)}, whose
    // closure = {(c,SC,d)}, so (c,SC,e) loses all support and is withdrawn; the
    // rule consequence (a,TYPE,e) goes with it.
    circuit.retract_triple((D, SC, E));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        0,
        "(c,SC,e) withdrawn — its base support (d,SC,e) is gone, no alternate path"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, E)),
        0,
        "(a,TYPE,e) withdrawn — its only support (closure edge (c,SC,e)) is gone"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        1,
        "(a,TYPE,d) survives — (c,SC,d) remains asserted"
    );
}

/// Overlap, both supports lost: the same triple `(1,SC,3)` is produced by BOTH
/// a transitive SC closure plan and a transitive SC rule. Retracting `(2,SC,3)`
/// removes the shared base edge: the rule no longer derives `(1,SC,3)` (base SC
/// after = `{(1,SC,2)}`), AND the closure plan withdraws it (its closure is
/// `{(1,SC,2)}`). With closure-path retraction live, the row loses BOTH supports
/// and must be withdrawn — the previous behavior (closure kept it alive) is gone.
#[test]
fn overlap_triple_withdrawn_when_shared_support_retracted() {
    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(TransitiveOn {
        id: R1_SCM_SCO,
        p: SC,
    }));
    circuit.add_plan(plan, R1_SCM_SCO);

    circuit.assert_triple((1, SC, 2));
    circuit.assert_triple((2, SC, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, SC, 3)),
        1,
        "both rule and closure derive (1,SC,3)"
    );

    // Retract the shared base edge (2,SC,3). The closure withdraws (1,SC,3)
    // [closure of {(1,SC,2)} = {(1,SC,2)}] and the rule no longer derives it,
    // so the row loses all support.
    circuit.retract_triple((2, SC, 3));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(1, SC, 3)),
        0,
        "(1,SC,3) withdrawn — both rule and closure support lost"
    );
}

/// Ghost case: a closure plan emits a *direct* edge `(c,SC,d)` that is ALSO
/// asserted by the user, so the edge is materialized only in `asserted_base`
/// (the closure pass's dedup-skip means it never lands in `derived_base`). A
/// rule `(a TYPE c) ∧ (c SC d) → (a TYPE d)` derives `(a,TYPE,d)` off that
/// asserted edge.
///
/// When the asserted `(c,SC,d)` is retracted, its only support disappears —
/// the closure plan does NOT independently materialize `(c,SC,d)` as a
/// non-asserted derived row — so the rule consequence `(a,TYPE,d)` MUST be
/// withdrawn.
///
/// Before the `closure_support ⊆ derived_base` fix, the closure pass recorded
/// `(c,SC,d)` in `closure_support` unconditionally (even though it lived only
/// in `asserted_base`). After retraction that ghost seeded
/// `recompute_rule_closure`, re-deriving `(a,TYPE,d)` from a triple the
/// materialized store says is gone. This test pins that the consequence is
/// now withdrawn.
#[test]
fn stale_rule_consequence_on_retracted_asserted_edge_is_withdrawn() {
    const A: u64 = 5;
    const C: u64 = 1;
    const D: u64 = 2;

    let mut circuit = Circuit::new();
    // SC closure plan: emits direct edges too, so an asserted (c,SC,d) is
    // re-emitted by the closure pass on its insertion tick.
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(CaxScoRule { id: R3_CAX_SCO }));
    circuit.add_plan(plan, R3_CAX_SCO);

    // Assert the SC edge (c,SC,d) and the type (a,TYPE,c). The rule derives
    // (a,TYPE,d) from the asserted SC edge.
    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        1,
        "rule derives (a,TYPE,d) from the asserted (c,SC,d)"
    );
    // (c,SC,d) is asserted, so the closure pass's dedup-skip keeps it out of
    // derived_base — it lives only in asserted_base.
    assert_eq!(
        circuit.derived_base().get(&(C, SC, D)),
        0,
        "(c,SC,d) is asserted, not materialized in derived_base"
    );

    // Retract the asserted (c,SC,d). Its only support is gone; the closure
    // plan does not independently materialize it as a derived row, so the
    // rule consequence (a,TYPE,d) must be withdrawn.
    circuit.retract_triple((C, SC, D));
    circuit.tick();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, D)),
        0,
        "(c,SC,d) must not be a ghost materialized row after retraction"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, TYPE, D)),
        0,
        "(a,TYPE,d) must be withdrawn — its only support (asserted (c,SC,d)) is gone"
    );
}

/// A rule whose body matches the SPECIFIC closure edge `(c,SC,e)` and emits a
/// fresh-predicate head `(a,GOAL,e)` that cannot feed back into any rule. The
/// consequence is therefore derivable ONLY when the closure edge `(c,SC,e)` is
/// present in the extent rules join against (owned via `closure_support`); the base SC edges
/// `(c,SC,d),(c,SC,x),(x,SC,e)` do NOT let the rule reconstruct it, because
/// there is no transitive SC rule registered — only the closure plan closes SC.
struct GoalOnClosureEdge {
    id: horndb_incremental::RuleId,
    a: u64,
    c: u64,
    e: u64,
}

const GOAL: u64 = 250;

impl horndb_incremental::BilinearRule for GoalOnClosureEdge {
    fn id(&self) -> horndb_incremental::RuleId {
        self.id
    }
    fn apply_full(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        let mut out = horndb_incremental::Zset::new();
        // Fire (a,GOAL,e) iff BOTH (a,TYPE,c) and the exact (c,SC,e) are present.
        let have_type = a.get(&(self.a, TYPE, self.c)) > 0;
        let have_edge = a.get(&(self.c, SC, self.e)) > 0;
        if have_type && have_edge {
            out.add((self.a, GOAL, self.e), 1);
        }
        out
    }
    fn apply_delta(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        da: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _db: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        // Exact delta of the presence-driven head: F(base ∪ delta) − F(base),
        // i.e. +1 when the pair becomes jointly present, −1 when it stops.
        // PLAN-24-01: the weight-trace circuit requires the documented delta
        // contract; the previous version re-emitted an already-present head
        // on every call (harmless under the Stage-1 "newly present" dedup,
        // but a spurious weight increment for the incremental distinct).
        let mut post = a.clone();
        post.add_assign(da);
        let mut out = self.apply_full(&post, &post);
        out.sub_assign(&self.apply_full(a, a));
        out
    }
}

/// Finding 2 — same-tick insert+retract: a rule consequence that depends on a
/// closure edge must SURVIVE a tick that retracts one support edge AND inserts a
/// replacement path keeping the closure edge entailed.
///
/// Setup (closure SC plan + a rule that fires ONLY off the closure edge
/// `(c,SC,e)`; no transitive SC rule, so the closure plan alone produces
/// `(c,SC,e)`):
///   base SC: (c,SC,d),(d,SC,e)  →  closure (c,SC,e)
///   (a,TYPE,c)  →  GoalOnClosureEdge derives (a,GOAL,e) off the closure edge.
///
/// Mixed tick: retract (d,SC,e) AND insert a replacement path (c,SC,x),(x,SC,e).
/// Post-tick the base SC closure still entails (c,SC,e) [via c->x->e], so the
/// rule consequence (a,GOAL,e) must SURVIVE.
///
/// The closure insertion pass runs BEFORE the rule fixpoint — on every tick
/// kind under the unified tick (PLAN-24-01, divergence 3) — so the extent the
/// rules join against already contains the re-derived (c,SC,e). Without that
/// ordering, the retraction pass withdrew (c,SC,e), the Stage-1 recompute
/// withdrew (a,GOAL,e), and the late insertion pass re-added (c,SC,e) only
/// AFTER rules had run.
#[test]
fn mixed_tick_insert_replacement_path_keeps_rule_consequence() {
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;
    const X: u64 = 5;

    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(GoalOnClosureEdge {
        id: R3_CAX_SCO,
        a: A,
        c: C,
        e: E,
    }));
    circuit.add_plan(plan, R3_CAX_SCO);

    // Tick 1: the closure chain only — closure derives (c,SC,e). (Separate
    // ticks are no longer required: the unified tick runs the closure
    // insertion pass before the rule fixpoint on every tick kind (PLAN-24-01,
    // divergence 3), so a rule can fire off a same-tick closure edge. Kept
    // split to stage the scenario explicitly.)
    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((D, SC, E));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "closure derives (c,SC,e)"
    );

    // Tick 2: now assert (a,TYPE,c) — the rule fires off the existing (c,SC,e).
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(A, GOAL, E)),
        1,
        "rule derives (a,GOAL,e) off the closure edge (c,SC,e)"
    );

    // Mixed tick: retract (d,SC,e) AND insert the replacement path c->x->e.
    circuit.retract_triple((D, SC, E));
    circuit.assert_triple((C, SC, X));
    circuit.assert_triple((X, SC, E));
    circuit.tick();

    // Post-tick the closure still entails (c,SC,e) via c->x->e.
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "(c,SC,e) still entailed via the replacement path c->x->e"
    );
    // The rule consequence off the closure edge must SURVIVE the mixed tick.
    assert_eq!(
        circuit.derived_base().get(&(A, GOAL, E)),
        1,
        "(a,GOAL,e) must survive — its support (c,SC,e) is still entailed post-tick"
    );
}

/// A rule that derives the fixed head `(C,SC,E)` whenever an asserted marker
/// `(C,MARK,E)` is present — an INDEPENDENT support for `(C,SC,E)` that does NOT
/// rely on any SC edge. Used by the Finding-3 test to remove the rule support
/// (retract the marker) while leaving the closure chain support intact.
struct MarkerRule {
    id: horndb_incremental::RuleId,
    c: u64,
    e: u64,
}

const MARK: u64 = 200;

impl horndb_incremental::BilinearRule for MarkerRule {
    fn id(&self) -> horndb_incremental::RuleId {
        self.id
    }
    fn apply_full(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        let mut out = horndb_incremental::Zset::new();
        // Unary trigger expressed as a bilinear self-join: emit (c,SC,e) once
        // per present marker (c,MARK,e). We scan only `a` and ignore `b` so the
        // head's multiplicity tracks marker presence (the circuit's
        // set-semantics distinct collapses it to 1).
        for ((xs, xp, xo), m) in a.iter() {
            if *xp == MARK && *xs == self.c && *xo == self.e && m > 0 {
                out.add((self.c, SC, self.e), 1);
            }
        }
        out
    }
    fn apply_delta(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        da: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _db: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        // Exact delta of the marker-presence head: F(base ∪ delta) − F(base).
        // PLAN-24-01: the weight-trace circuit requires the documented delta
        // contract; the previous version (`apply_full(da, da)`) filtered
        // `m > 0`, so a marker RETRACTION produced no delta and left a stale
        // positive weight in the trace.
        let mut post = a.clone();
        post.add_assign(da);
        let mut out = self.apply_full(&post, b);
        out.sub_assign(&self.apply_full(a, b));
        out
    }
}

/// Finding 3 — record closure ownership for rule-owned promotions.
///
/// `(c,SC,e)` is simultaneously: asserted-direct, path-implied (via the chain
/// `(c,SC,d),(d,SC,e)`), AND derived by an INDEPENDENT rule off a marker
/// `(c,MARK,e)`. We retract the direct assertion (Step A) — this PROMOTES
/// `(c,SC,e)` (still entailed by the closure chain) while the marker rule ALSO
/// owns the derived row. Finding 3: the promote loop sees the row already in
/// `derived_base` (rule-owned) and must STILL record `closure_support` — not
/// treat it as a no-op. Then (Step B) we retract the MARKER, removing the rule
/// support entirely; the closure chain `(c,SC,d),(d,SC,e)` still entails
/// `(c,SC,e)`, so the row MUST PERSIST via `closure_support`.
///
/// This pins the end-to-end contract: a promoted survivor that is also
/// rule-derived survives the loss of its rule support because the closure still
/// entails it. The Finding-3 fix makes the promote loop record `closure_support`
/// even when the row is already materialized in `derived_base` (rule-owned),
/// rather than treating it as a no-op — closing a latent ownership gap.
#[test]
fn rule_owned_promotion_records_closure_support() {
    const C: u64 = 1;
    const D: u64 = 2;
    const E: u64 = 3;

    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(MarkerRule {
        id: R3_CAX_SCO,
        c: C,
        e: E,
    }));
    circuit.add_plan(plan, R3_CAX_SCO);

    // Closure chain + the marker (independent rule support) + the direct edge.
    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((D, SC, E));
    circuit.assert_triple((C, MARK, E)); // independent rule support
    circuit.assert_triple((C, SC, E)); // direct (asserted) edge
    circuit.tick();

    // (c,SC,e) is asserted → lives in asserted_base, not derived_base.
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        0,
        "(c,SC,e) is asserted, not materialized in derived_base"
    );

    // Step A: retract the direct (c,SC,e). Still entailed by the closure chain
    // AND derived by the marker rule. It must become a materialized derived row,
    // and Finding 3 requires closure_support to record ownership too.
    circuit.retract_triple((C, SC, E));
    circuit.tick();
    assert!(
        circuit.derived_base().get(&(C, SC, E)) > 0,
        "(c,SC,e) must persist after retracting the direct edge; got {}",
        circuit.derived_base().get(&(C, SC, E))
    );

    // Step B: retract the MARKER — removes the rule support entirely. The
    // closure chain (c,SC,d),(d,SC,e) still entails (c,SC,e), so closure_support
    // MUST keep the row alive. Before the Finding-3 fix it was wrongly zeroed.
    circuit.retract_triple((C, MARK, E));
    circuit.tick();
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "(c,SC,e) MUST persist via closure_support after the rule support is gone"
    );
}

/// Cyclic self-support for the exact edge `(c,SC,e)`: the body consumes the
/// rule's own head — `(c,SC,e) ∧ (c,LOOP,e) → (c,SC,e)`. Its weight is
/// positive whenever the head itself sits in the extent and the loop trigger
/// is present: pure cyclic support that can never bootstrap the row from
/// scratch. Used to pin the dual-ownership orphan bug (codex P1): a row whose
/// only remaining rule weight is this cycle must die when its closure support
/// is retracted.
struct SelfSupportOnLoop {
    id: horndb_incremental::RuleId,
    c: u64,
    e: u64,
}

const LOOP: u64 = 300;

impl horndb_incremental::BilinearRule for SelfSupportOnLoop {
    fn id(&self) -> horndb_incremental::RuleId {
        self.id
    }
    fn apply_full(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        let mut out = horndb_incremental::Zset::new();
        if a.get(&(self.c, SC, self.e)) > 0 && a.get(&(self.c, LOOP, self.e)) > 0 {
            out.add((self.c, SC, self.e), 1);
        }
        out
    }
    fn apply_delta(
        &self,
        a: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _b: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        da: &horndb_incremental::Zset<horndb_incremental::TripleId>,
        _db: &horndb_incremental::Zset<horndb_incremental::TripleId>,
    ) -> horndb_incremental::Zset<horndb_incremental::TripleId> {
        // Exact delta contract: F(post) − F(pre).
        let mut post = a.clone();
        post.add_assign(da);
        let mut out = self.apply_full(&post, &post);
        out.sub_assign(&self.apply_full(a, a));
        out
    }
}

/// Build the codex-P1 dual-ownership circuit and drive it to the state where
/// `X = (C,SC,E)` is rule-owned (`rule_attr`) with ONLY cyclic rule weight
/// left, while the closure chain `(C,SC,D),(D,SC,E)` entails it
/// (`closure_support`). Constants: A=1, C=2, D=3, E=4.
///
/// Ticks: (1) marker + loop trigger + type — MarkerRule materializes X
/// rule-owned; SelfSupportOnLoop adds the cyclic weight; GoalOnClosureEdge
/// derives (A,GOAL,E) off X. (2) assert the chain — the closure entails X and
/// records dual ownership. (3) retract the marker — X survives (closure
/// covers it) but its only rule weight is now the cycle.
fn dual_owned_cyclic_circuit() -> Circuit {
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;

    let mut circuit = Circuit::new();
    circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(SC)));
    let mut p1 = NaryPlan::new();
    p1.push_join(Box::new(MarkerRule {
        id: R1_SCM_SCO,
        c: C,
        e: E,
    }));
    circuit.add_plan(p1, R1_SCM_SCO);
    let mut p2 = NaryPlan::new();
    p2.push_join(Box::new(SelfSupportOnLoop {
        id: R2_SCM_SPO,
        c: C,
        e: E,
    }));
    circuit.add_plan(p2, R2_SCM_SPO);
    let mut p3 = NaryPlan::new();
    p3.push_join(Box::new(GoalOnClosureEdge {
        id: R3_CAX_SCO,
        a: A,
        c: C,
        e: E,
    }));
    circuit.add_plan(p3, R3_CAX_SCO);

    // Tick 1: external (well-founded) rule support + cyclic trigger + type.
    circuit.assert_triple((C, MARK, E));
    circuit.assert_triple((C, LOOP, E));
    circuit.assert_triple((A, TYPE, C));
    circuit.tick();
    circuit.debug_validate();
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "MarkerRule materializes X rule-owned"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, GOAL, E)),
        1,
        "consequence (A,GOAL,E) derived off X"
    );

    // Tick 2: the closure chain also entails X → dual ownership.
    circuit.assert_triple((C, SC, D));
    circuit.assert_triple((D, SC, E));
    circuit.tick();
    circuit.debug_validate();
    assert_eq!(circuit.derived_base().get(&(C, SC, E)), 1);

    // Tick 3: retract the marker. X correctly survives — the closure still
    // entails it — but its only remaining rule weight is the cycle.
    circuit.retract_triple((C, MARK, E));
    circuit.tick();
    circuit.debug_validate();
    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "X survives the marker retraction via closure support"
    );
    assert_eq!(circuit.derived_base().get(&(A, GOAL, E)), 1);
    circuit
}

/// Codex P1 — a dual-owned row that loses its closure support must be
/// re-checked by the rule fixpoint, not kept alive by cyclic-only rule weight.
///
/// After `dual_owned_cyclic_circuit` (X rule-owned, cyclic-only rule weight,
/// closure-covered), retracting the chain edge `(D,SC,E)` removes the
/// closure's entailment of X. The closure retract pass strips
/// `closure_support` but X is `rule_attr`-owned, so nothing withdraws the
/// row there — the fixpoint must be seeded to re-check it. X's remaining
/// weight is purely cyclic (SelfSupportOnLoop consumes X itself), so the
/// two-phase fixpoint must kill X AND its consequence (A,GOAL,E).
#[test]
fn dual_owned_row_dies_when_closure_support_later_retracted() {
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;
    let mut circuit = dual_owned_cyclic_circuit();

    // Tick 4: retract the chain edge — closure entailment of X vanishes.
    circuit.retract_triple((D, SC, E));
    circuit.tick();
    circuit.debug_validate();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        0,
        "X must die — marker gone, closure gone, remaining rule weight is cyclic-only"
    );
    assert_eq!(
        circuit.derived_base().get(&(A, GOAL, E)),
        0,
        "the consequence of X must be withdrawn with it"
    );
    // Differential oracle: the well-founded recompute over the surviving
    // asserted base derives nothing; derived_base must agree.
    assert!(
        circuit.oracle_rule_closure().is_empty(),
        "oracle derives nothing from {{loop, type, (C,SC,D)}}"
    );
    assert!(
        circuit.derived_base().iter().all(|(_, m)| m <= 0),
        "no derived row survives; derived = {:?}",
        circuit.derived_base().iter().collect::<Vec<_>>()
    );
}

/// Mixed-tick variant: the SAME tick retracts the closure chain edge AND
/// asserts a replacement path, so X stays closure-entailed throughout. The
/// orphan reseed must NOT overdelete X (it is re-covered by the Phase-3
/// insertion pass before the fixpoint runs), and X must produce no feed
/// event. A later genuine loss of the replacement path must then kill X.
#[test]
fn mixed_tick_closure_reroute_keeps_dual_owned_row() {
    const A: u64 = 1;
    const C: u64 = 2;
    const D: u64 = 3;
    const E: u64 = 4;
    const F: u64 = 6;
    let mut circuit = dual_owned_cyclic_circuit();

    // Mixed tick: retract (D,SC,E) AND assert the replacement path C->F->E.
    let rx = circuit.subscribe();
    circuit.retract_triple((D, SC, E));
    circuit.assert_triple((C, SC, F));
    circuit.assert_triple((F, SC, E));
    circuit.tick();
    circuit.debug_validate();

    assert_eq!(
        circuit.derived_base().get(&(C, SC, E)),
        1,
        "X survives — the replacement path keeps it closure-entailed"
    );
    assert_eq!(circuit.derived_base().get(&(A, GOAL, E)), 1);
    // No derived feed transient for X: the withdraw was rule-owned (skipped)
    // and the re-cover only records ownership on the already-present row.
    while let Ok(rec) = rx.try_recv() {
        assert!(
            rec.triple != (C, SC, E),
            "no feed event for the re-covered row; saw {rec:?}"
        );
    }

    // Now genuinely remove the replacement path: X loses closure support with
    // only cyclic rule weight left → X and its consequence die.
    circuit.retract_triple((F, SC, E));
    circuit.tick();
    circuit.debug_validate();
    assert_eq!(circuit.derived_base().get(&(C, SC, E)), 0);
    assert_eq!(circuit.derived_base().get(&(A, GOAL, E)), 0);
    assert!(circuit.oracle_rule_closure().is_empty());
}
