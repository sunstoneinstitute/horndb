//! Operator traits. SPEC-06 F2 (linear), F3 (bilinear), F4 (n-ary).
//!
//! These traits are the contract between this crate and SPEC-04 (rule
//! codegen). Adding a method here is a coordinated workspace change.
//!
//! Stage 1 covers insertion-only correctness. Negative-multiplicity
//! inputs are accepted; bilinear retraction across joins is a Stage 2
//! deliverable (F6 in SPEC-06).

use crate::extent::PredExtent;
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

    /// The predicate each leaf (`[left, right]`) reads from the base
    /// extent. `Some(p)` means the leaf's body pattern has `p` fixed in
    /// predicate position, so `NaryPlan` binds it to just that predicate's
    /// slice of the extent (SPEC-24 S7). `None` (the default) means the
    /// leaf's body pattern has a variable in predicate position (e.g.
    /// prp-dom's `(?x ?p ?y)`), so it must still read the whole extent.
    fn body_predicates(&self) -> [Option<u64>; 2] {
        [None, None]
    }
}

/// F4: n-ary rule planner.
///
/// Stage 1: left-deep tree of bilinear joins. `push_join(rule)` appends
/// a join whose left input is the running intermediate and whose right
/// input is the base extent. Cost-based reordering is a Stage 2
/// deliverable.
///
/// Each leaf binds to the predicate slice its rule declares via
/// `BilinearRule::body_predicates` (SPEC-24 S7) — level 0 slices both
/// sides, level `i >= 1` slices only its right side (its left side is the
/// running intermediate, never sliced). A rule that declares `None` for a
/// side reads the whole extent there, unchanged from Stage 1.
pub struct NaryPlan {
    joins: Vec<Box<dyn BilinearRule>>,
    /// Integrated left-input intermediates for joins[1..] (z⁻¹ state).
    /// None until the first stateful call (lazy cold-start from the base
    /// passed to that call). state[i] is the left input of joins[i+1].
    state: Option<Vec<Zset<TripleId>>>,
}

/// The slice of `ext` a leaf declaring `pred` reads: `Some(p)` binds to
/// just `p`'s rows, `None` reads the whole extent.
fn leaf(ext: &PredExtent, pred: Option<u64>) -> &Zset<TripleId> {
    match pred {
        Some(p) => ext.slice(p),
        None => ext.all(),
    }
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
    /// base extent, each leaf bound to its declared predicate slice.
    pub fn apply_full(&self, base: &PredExtent) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return base.all().clone();
        }
        let [lp0, rp0] = self.joins[0].body_predicates();
        let mut intermediate = self.joins[0].apply_full(leaf(base, lp0), leaf(base, rp0));
        for rule in &self.joins[1..] {
            let rp = rule.body_predicates()[1];
            intermediate = rule.apply_full(&intermediate, leaf(base, rp));
        }
        intermediate
    }

    /// Delta eval: each join is reduced via F3, the intermediates flow
    /// through as both base and delta inputs to the next join. `delta` is
    /// sliced the same way `base` is (built into a `PredExtent` once up
    /// front) so a leaf never sees rows outside its declared predicate.
    pub fn apply_delta(&self, base: &PredExtent, delta: &Zset<TripleId>) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return delta.clone();
        }
        let delta_ext = PredExtent::from_zset(delta);
        let [lp0, rp0] = self.joins[0].body_predicates();
        let base_l = leaf(base, lp0);
        let base_r = leaf(base, rp0);
        let mut int_base = self.joins[0].apply_full(base_l, base_r);
        let mut int_delta =
            self.joins[0].apply_delta(base_l, base_r, leaf(&delta_ext, lp0), leaf(&delta_ext, rp0));
        for rule in &self.joins[1..] {
            let rp = rule.body_predicates()[1];
            let base_r = leaf(base, rp);
            let delta_r = leaf(&delta_ext, rp);
            let next_base = rule.apply_full(&int_base, base_r);
            let next_delta = rule.apply_delta(&int_base, base_r, &int_delta, delta_r);
            int_base = next_base;
            int_delta = next_delta;
        }
        int_delta
    }

    /// Stateful delta eval (DBSP z⁻¹ construction): each level's left
    /// input is an integrated intermediate held in `state`, updated in
    /// place instead of recomputed via `apply_full` on every call. This
    /// makes the per-tick cost proportional to the delta, not the extent.
    /// Each level's right input (base and delta alike) is bound to its
    /// rule's declared predicate slice (SPEC-24 S7).
    ///
    /// `base` must be the pre-delta extent at every call — same "old-old"
    /// convention as `apply_delta`: `Δ(A⋈B) = ΔA⋈B_old + A_old⋈ΔB +
    /// ΔA⋈ΔB`. The caller folds `delta` into its own extent only after
    /// this call returns. On the first call (or after `reset_state`),
    /// `state` is lazily rebuilt from `base` by the same left fold
    /// `apply_full` uses.
    pub fn apply_delta_stateful(
        &mut self,
        base: &PredExtent,
        delta: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        if self.joins.is_empty() {
            return delta.clone();
        }
        let delta_ext = PredExtent::from_zset(delta);
        if self.state.is_none() {
            let mut intermediates = Vec::new();
            if self.joins.len() > 1 {
                let [lp0, rp0] = self.joins[0].body_predicates();
                let mut prev = self.joins[0].apply_full(leaf(base, lp0), leaf(base, rp0));
                intermediates.push(prev.clone());
                for rule in &self.joins[1..self.joins.len() - 1] {
                    let rp = rule.body_predicates()[1];
                    prev = rule.apply_full(&prev, leaf(base, rp));
                    intermediates.push(prev.clone());
                }
            }
            self.state = Some(intermediates);
        }
        let state = self.state.as_mut().expect("initialized above");

        // Level 0: both inputs are the shared base — no stored state.
        let [lp0, rp0] = self.joins[0].body_predicates();
        let mut prev_delta = self.joins[0].apply_delta(
            leaf(base, lp0),
            leaf(base, rp0),
            leaf(&delta_ext, lp0),
            leaf(&delta_ext, rp0),
        );
        // Levels 1..: state[i] is the left input for joins[i + 1].
        for (i, rule) in self.joins[1..].iter().enumerate() {
            let rp = rule.body_predicates()[1];
            let next_delta =
                rule.apply_delta(&state[i], leaf(base, rp), &prev_delta, leaf(&delta_ext, rp));
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
