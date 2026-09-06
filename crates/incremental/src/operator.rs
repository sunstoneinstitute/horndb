//! Operator traits. SPEC-06 F2 (linear), F3 (bilinear), F4 (n-ary).
//!
//! These traits are the contract between this crate and SPEC-04 (rule
//! codegen). Adding a method here is a coordinated workspace change.
//!
//! Stage 1 covers insertion-only correctness. Negative-multiplicity
//! inputs are accepted; bilinear retraction across joins is a Stage 2
//! deliverable (F6 in SPEC-06).

use horndb_wcoj::estimator::StatsEstimator;
use horndb_wcoj::stats::Stats;
use horndb_wcoj::{Term, TriplePattern, Var};

use crate::extent::PredExtent;
use crate::kernels::{vars_in, HashJoinRule, KernelError};
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
///
/// [`crate::kernels::HashJoinRule`] is the reference runtime: a generic
/// hash join parameterised by two body patterns and a head pattern,
/// correct for arbitrary multiplicities (SPEC-24 §S7 leaf 2). Construct
/// one instead of hand-writing a new nested loop against this trait.
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
/// A left-deep tree of bilinear joins. `push_join(rule)` appends a join
/// whose left input is the running intermediate and whose right input is
/// the base extent, in the order the caller pushes.
///
/// [`NaryPlan::from_body`] picks that order instead of taking it from the
/// caller: given a rule body as triple patterns it greedily builds a
/// connected left-deep order that minimises the estimated intermediate
/// size, costed over the SPEC-23 §5.3 [`Stats`] seam (SPEC-06 F4,
/// SPEC-24 §S7). Reordering only changes the cost — the plan derives the
/// same rows either way.
///
/// Each leaf binds to the predicate slice its rule declares via
/// `BilinearRule::body_predicates` (SPEC-24 S7) — level 0 slices both
/// sides, level `i >= 1` slices only its right side (its left side is the
/// running intermediate, never sliced). A rule that declares `None` for a
/// side reads the whole extent there, unchanged from Stage 1.
pub struct NaryPlan {
    joins: Vec<Box<dyn BilinearRule>>,
    /// Body indices in the order [`NaryPlan::from_body`] joined them.
    /// Empty for a plan assembled with [`NaryPlan::push_join`].
    leaf_order: Vec<usize>,
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
            leaf_order: Vec::new(),
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

    /// The body indices this plan joins, in join order — the planner's
    /// choice, exposed for tests and EXPLAIN. Empty for a plan assembled
    /// with [`NaryPlan::push_join`].
    pub fn leaf_order(&self) -> &[usize] {
        &self.leaf_order
    }

    /// Build a left-deep plan for `body` in a cost-chosen order
    /// (SPEC-06 F4, SPEC-24 §S7).
    ///
    /// Every level is a [`HashJoinRule`]: level 0 joins the two cheapest
    /// connected patterns, each later level joins the running
    /// intermediate against the next chosen pattern. The last level's
    /// head is `head`; every earlier level's head is the three-slot
    /// projection of the prefix's *live* variables (see the private
    /// `intermediate_shape` helper).
    ///
    /// A single-pattern body is [`LinearRule`]'s job, not this planner's,
    /// so `body.len() < 2` is an error. So is a body whose patterns do
    /// not form one connected component (cross products are not planned)
    /// and a prefix needing more than three live variables.
    pub fn from_body(
        id: RuleId,
        body: &[TriplePattern],
        head: TriplePattern,
        stats: &dyn Stats,
    ) -> Result<NaryPlan, KernelError> {
        if body.len() < 2 {
            return Err(KernelError::BodyTooShort(body.len()));
        }
        let order = choose_order(body, stats)?;
        let mut plan = NaryPlan::new();
        let mut left = body[order[0]];
        for step in 0..order.len() - 1 {
            let right = body[order[step + 1]];
            let step_head = if step + 2 == order.len() {
                head
            } else {
                intermediate_shape(&left, &right, body, &order[step + 2..], &head)?
            };
            plan.push_join(Box::new(HashJoinRule::new(id, left, right, step_head)?));
            left = step_head;
        }
        plan.leaf_order = order;
        Ok(plan)
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

/// Value the unused slots of an intermediate shape carry. The shape is
/// both the head a level writes and the left pattern the next level
/// matches, so a `Bound` pad round-trips: the level writes it, the next
/// level matches it back.
const INTERMEDIATE_PAD: u64 = 0;

/// The three-slot shape a join prefix hands to the next level: the
/// prefix's *live* variables — those some later body pattern or the final
/// head still needs — in first-appearance order across `left` then
/// `right`, unused slots padded.
///
/// The live variables keep their own [`Var`] ids rather than getting
/// fresh ones, so the `Var` -> slot map is the identity: the next level's
/// left pattern *is* this pattern, and matching an intermediate row
/// against it rebinds each variable to the value this level wrote.
///
/// `NaryPlan` threads intermediates as `Zset<TripleId>`, so a prefix
/// needing a fourth live variable has no shape — that is
/// [`HashJoinRule`]'s documented ceiling, reported as
/// [`KernelError::TooManyLiveVars`].
fn intermediate_shape(
    left: &TriplePattern,
    right: &TriplePattern,
    body: &[TriplePattern],
    rest: &[usize],
    head: &TriplePattern,
) -> Result<TriplePattern, KernelError> {
    let mut still_needed = vars_in(head);
    for &i in rest {
        for v in vars_in(&body[i]) {
            if !still_needed.contains(&v) {
                still_needed.push(v);
            }
        }
    }
    let mut live: Vec<Var> = Vec::new();
    for v in vars_in(left).into_iter().chain(vars_in(right)) {
        if still_needed.contains(&v) && !live.contains(&v) {
            live.push(v);
        }
    }
    if live.len() > 3 {
        return Err(KernelError::TooManyLiveVars(live.len()));
    }
    let slot = |i: usize| {
        live.get(i)
            .copied()
            .map_or(Term::Bound(INTERMEDIATE_PAD), Term::Var)
    };
    Ok(TriplePattern::new(slot(0), slot(1), slot(2)))
}

/// Greedy connected left-deep leaf ordering: start from the cheapest
/// single pattern, then repeatedly append the unused pattern that shares
/// a variable with the prefix and minimises the estimated size of
/// `prefix ∪ {p}`. Ties break by declaration order.
///
/// Uninformed statistics (`ZeroStats`) carry no per-predicate signal, so
/// costing would only break ties — the walk then keeps declaration order,
/// mirroring `horndb_wcoj::planner`, which also skips the cost search
/// there.
///
/// ponytail: greedy, O(n²) estimator calls, no bushy trees. Fine for the
/// handful of patterns an OWL 2 RL rule body has. Upgrade path if a body
/// ever exceeds five patterns: SPEC-23 §5.5's connected-subset DP
/// (`horndb_wcoj::planner::Planner::choose`).
fn choose_order(body: &[TriplePattern], stats: &dyn Stats) -> Result<Vec<usize>, KernelError> {
    let estimator = StatsEstimator::new(stats);
    let costed = stats.is_informed();
    let start = if costed {
        (0..body.len())
            .min_by_key(|&i| estimator.estimate_pattern(&body[i]).estimate)
            .expect("body has at least two patterns")
    } else {
        0
    };
    let mut order = vec![start];
    let mut prefix = vec![body[start]];
    while order.len() < body.len() {
        let prefix_vars: Vec<Var> = prefix.iter().flat_map(vars_in).collect();
        let mut best: Option<(u64, usize)> = None;
        for (i, pat) in body.iter().enumerate() {
            if order.contains(&i) || !vars_in(pat).iter().any(|v| prefix_vars.contains(v)) {
                continue;
            }
            let cost = if costed {
                let mut candidate = prefix.clone();
                candidate.push(*pat);
                estimator.estimate_bgp(&candidate).estimate
            } else {
                0
            };
            if best.is_none_or(|(best_cost, _)| cost < best_cost) {
                best = Some((cost, i));
            }
        }
        let (_, pick) = best.ok_or(KernelError::DisconnectedBody)?;
        order.push(pick);
        prefix.push(body[pick]);
    }
    Ok(order)
}

impl Default for NaryPlan {
    fn default() -> Self {
        Self::new()
    }
}
