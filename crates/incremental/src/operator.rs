//! Operator traits. SPEC-06 F2 (linear), F3 (bilinear), F4 (n-ary).
//!
//! These traits are the contract between this crate and SPEC-04 (rule
//! codegen). Adding a method here is a coordinated workspace change.
//!
//! Stage 1 covers insertion-only correctness. Negative-multiplicity
//! inputs are accepted; bilinear retraction across joins is a Stage 2
//! deliverable (F6 in SPEC-06).

use crate::types::{RuleId, TripleId};
use crate::zset::Zset;

/// F2: a rule whose body is a single triple pattern.
///
/// Linearity: `apply_delta(a + b) = apply_delta(a) + apply_delta(b)`.
/// Property-checked in `tests/linear_rule.rs`.
pub trait LinearRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply_delta(&self, delta: &Zset<TripleId>) -> Zset<TripleId>;
}

/// F3: a rule whose body is a conjunction of two triple patterns.
///
/// DBSP decomposition: `Δ(A ⋈ B) = Δ_A ⋈ B + A ⋈ Δ_B + Δ_A ⋈ Δ_B`.
/// SPEC-04 codegen emits both `apply_full` (cold/Reset path) and
/// `apply_delta` (steady-state path).
pub trait BilinearRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId>;
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId>;
}

/// F4: n-ary rule planner.
///
/// Stage 1: left-deep tree of bilinear joins. `push_join(rule)` appends
/// a join whose left input is the running intermediate and whose right
/// input is the base extent. Cost-based reordering is a Stage 2
/// deliverable.
///
/// All patterns currently bind against the same base extent — the
/// caller is responsible for slicing per-predicate inputs upstream. A
/// per-leaf-input variant is a Stage 2 extension if SPEC-04 finds
/// rules with bodies spanning different predicate partitions.
pub struct NaryPlan {
    joins: Vec<Box<dyn BilinearRule>>,
}

impl NaryPlan {
    pub fn new() -> Self {
        Self { joins: Vec::new() }
    }
    pub fn push_join(&mut self, rule: Box<dyn BilinearRule>) {
        self.joins.push(rule);
    }
    pub fn arity(&self) -> usize {
        self.joins.len() + 1
    }

    /// Cold-start eval: fold the joins left-to-right starting from the
    /// base extent.
    pub fn apply_full(&self, base: &Zset<TripleId>) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return base.clone();
        }
        let mut intermediate = self.joins[0].apply_full(base, base);
        for rule in &self.joins[1..] {
            intermediate = rule.apply_full(&intermediate, base);
        }
        intermediate
    }

    /// Delta eval: each join is reduced via F3, the intermediates flow
    /// through as both base and delta inputs to the next join. Stage 1
    /// keeps the same `base` for every level for simplicity (correct
    /// when every body pattern reads the same predicate partition).
    pub fn apply_delta(&self, base: &Zset<TripleId>, delta: &Zset<TripleId>) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return delta.clone();
        }
        let mut int_base = self.joins[0].apply_full(base, base);
        let mut int_delta = self.joins[0].apply_delta(base, base, delta, delta);
        for rule in &self.joins[1..] {
            let next_base = rule.apply_full(&int_base, base);
            let next_delta = rule.apply_delta(&int_base, base, &int_delta, delta);
            int_base = next_base;
            int_delta = next_delta;
        }
        int_delta
    }
}

impl Default for NaryPlan {
    fn default() -> Self {
        Self::new()
    }
}
