//! Trivial nested-loop planner. SPEC-03 (WCOJ) will replace this in Stage 2;
//! for Stage 1 the plan shape is "iterate the leading pattern, probe the rest".

use crate::codegen::parse::{RuleSpec, Slot};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct Plan {
    /// Order in which body patterns are visited. Index into `rule.body`.
    #[allow(dead_code)]
    pub order: Vec<usize>,
    /// For each step (in `order` order): for each slot (s,p,o), is the slot
    /// `Bound` to a previously-named variable (or vocab), or does it introduce
    /// a new variable to bind?
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone)]
pub struct PlanStep {
    #[allow(dead_code)]
    pub pattern_index: usize,
    pub s: SlotPlan,
    pub p: SlotPlan,
    pub o: SlotPlan,
}

#[derive(Debug, Clone)]
pub enum SlotPlan {
    /// Slot is fixed: either to a vocabulary term or to a prior variable.
    /// In codegen this becomes `Some(<expr>)` to `store.probe`.
    Bound(BoundSource),
    /// Slot introduces a fresh variable named `name`. The probe sees `None`
    /// at this slot; the codegen reads the resulting triple's slot.
    Fresh(String),
    /// Slot's variable was already introduced *within the same pattern step*
    /// (e.g. `?x ?p ?x` — the second `?x`). Probe sees `None` at this slot
    /// and the emitter adds a post-probe equality filter against the slot
    /// that first introduced the variable (always an earlier slot in the
    /// same triple). `name` is retained for diagnostics.
    SameAsEarlierSlot {
        #[allow(dead_code)]
        name: String,
        earlier: SlotPos,
    },
}

/// Which slot in the current triple step first introduced a variable. Used
/// for the rare same-pattern repeated-variable case (e.g. `?x ?p ?x`).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SlotPos {
    S,
    P,
    O,
}

#[derive(Debug, Clone)]
pub enum BoundSource {
    Var(String),
    Vocab(&'static str),
}

pub fn plan_rule(rule: &RuleSpec) -> Plan {
    let mut bound: HashSet<String> = HashSet::new();
    let mut steps = Vec::with_capacity(rule.body.len());
    let order: Vec<usize> = (0..rule.body.len()).collect();
    for &idx in order.iter() {
        let pat = &rule.body[idx];
        // Track which variables this step has just introduced and at which
        // position, so a later slot in the same step that references the
        // same variable can be planned as `SameAsEarlierSlot`.
        let mut intra_step: Vec<(String, SlotPos)> = Vec::new();
        let s = classify(&pat.s, &mut bound, &mut intra_step, SlotPos::S);
        let p = classify(&pat.p, &mut bound, &mut intra_step, SlotPos::P);
        let o = classify(&pat.o, &mut bound, &mut intra_step, SlotPos::O);
        steps.push(PlanStep {
            pattern_index: idx,
            s,
            p,
            o,
        });
    }
    Plan { order, steps }
}

fn classify(
    slot: &Slot,
    bound: &mut HashSet<String>,
    intra_step: &mut Vec<(String, SlotPos)>,
    pos: SlotPos,
) -> SlotPlan {
    match slot {
        Slot::Vocab(v) => SlotPlan::Bound(BoundSource::Vocab(v.field)),
        Slot::Var(name) => {
            if let Some(&(_, earlier)) = intra_step.iter().find(|(n, _)| n == name) {
                // Same variable already appeared earlier in *this* triple step.
                // Plan as None probe + equality filter against the earlier slot.
                SlotPlan::SameAsEarlierSlot {
                    name: name.clone(),
                    earlier,
                }
            } else if bound.contains(name) {
                SlotPlan::Bound(BoundSource::Var(name.clone()))
            } else {
                bound.insert(name.clone());
                intra_step.push((name.clone(), pos));
                SlotPlan::Fresh(name.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::parse::parse_str;

    fn rule(src: &str) -> RuleSpec {
        parse_str(src).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn cax_sco_plan_has_two_steps() {
        let r = rule(
            r#"
            [[rule]]
            id = "cax-sco"
            body = [
              { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
              { s = "?x",  p = "rdf:type",        o = "?c1" },
            ]
            head = { s = "?x", p = "rdf:type", o = "?c2" }
        "#,
        );
        let plan = plan_rule(&r);
        assert_eq!(plan.steps.len(), 2);
        // Step 2's subject ?x is fresh; its object ?c1 is bound from step 1.
        match &plan.steps[1].o {
            SlotPlan::Bound(BoundSource::Var(n)) => assert_eq!(n, "c1"),
            other => panic!("expected ?c1 bound, got {other:?}"),
        }
    }
}
