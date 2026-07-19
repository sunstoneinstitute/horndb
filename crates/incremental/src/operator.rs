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
    /// Integrated left-input intermediates for joins[1..] (z⁻¹ state).
    /// None until the first stateful call (lazy cold-start from the base
    /// passed to that call). state[i] is the left input of joins[i+1].
    state: Option<Vec<Zset<TripleId>>>,
}

impl NaryPlan {
    pub fn new() -> Self {
        Self {
            joins: Vec::new(),
            state: None,
        }
    }
    /// Must not be called after the plan's first `apply_delta_stateful`
    /// round — the z⁻¹ `state` vector's length is fixed at first use and
    /// adding a join afterward leaves it too short.
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

    /// Stateful delta eval (DBSP z⁻¹ construction): each level's left
    /// input is an integrated intermediate held in `state`, updated in
    /// place instead of recomputed via `apply_full` on every call. This
    /// makes the per-tick cost proportional to the delta, not the extent.
    ///
    /// `base` must be the pre-delta extent at every call — same "old-old"
    /// convention as `apply_delta`: `Δ(A⋈B) = ΔA⋈B_old + A_old⋈ΔB +
    /// ΔA⋈ΔB`. The caller folds `delta` into its own extent only after
    /// this call returns. On the first call (or after `reset_state`),
    /// `state` is lazily rebuilt from `base` by the same left fold
    /// `apply_full` uses.
    pub fn apply_delta_stateful(
        &mut self,
        base: &Zset<TripleId>,
        delta: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return delta.clone();
        }
        if self.state.is_none() {
            let mut intermediates = Vec::new();
            if self.joins.len() > 1 {
                let mut prev = self.joins[0].apply_full(base, base);
                intermediates.push(prev.clone());
                for rule in &self.joins[1..self.joins.len() - 1] {
                    prev = rule.apply_full(&prev, base);
                    intermediates.push(prev.clone());
                }
            }
            self.state = Some(intermediates);
        }
        let state = self.state.as_mut().expect("initialized above");

        // Level 0: both inputs are the shared base — no stored state.
        let mut prev_delta = self.joins[0].apply_delta(base, base, delta, delta);
        // Levels 1..: state[i] is the left input for joins[i + 1].
        for (i, rule) in self.joins[1..].iter().enumerate() {
            let next_delta = rule.apply_delta(&state[i], base, &prev_delta, delta);
            // Fold this level's delta into its integrated intermediate
            // AFTER use — the delta rule needs the pre-round value.
            state[i].add_assign(&prev_delta);
            prev_delta = next_delta;
        }
        prev_delta
    }

    /// Clears the integrated per-level state, forcing the next
    /// `apply_delta_stateful` call to cold-start from its `base` argument.
    /// Used after a full-recompute fallback tick invalidates the traces.
    pub fn reset_state(&mut self) {
        self.state = None;
    }
}

impl Default for NaryPlan {
    fn default() -> Self {
        Self::new()
    }
}
