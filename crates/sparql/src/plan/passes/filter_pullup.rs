//! `FilterPullup` (SPEC-23 §5.2): hoist filters above inner joins so
//! `FilterPushdown` sees the complete conjunct set at each join. Conjuncts
//! are pulled through `Join` (both arms) only; every other node is a hard
//! boundary (`LeftJoin`, `Union`, `Distinct`, `Group`, `Slice`, ...).
//!
//! Hoisting is lattice-gated: a conjunct moves only when every variable it
//! references is provably bound ([`bound_vars`]) by its own arm. A filter
//! over a variable its arm never binds evaluates error→false there and
//! drops every row — but after the join the *other* arm may bind that
//! variable, so the hoisted filter could pass instead (and `Bound(?x)`
//! flips false→true outright). Such conjuncts stay exactly where they are.

use crate::algebra::Expr;
use crate::exec::runtime::referenced_vars;
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{bound_vars, conjoin, conjuncts, map_children};
use std::collections::HashSet;

pub struct FilterPullup;

impl LogicalPass for FilterPullup {
    fn id(&self) -> PassId {
        PassId::FilterPullup
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        pullup(plan)
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::Normalize]
    }
}

/// Bottom-up: pull up every child first, then — at a `Join` — peel any
/// immediate `Filter` wrapper off each (already-pulled-up) arm, hoist the
/// conjuncts whose variables the arm provably binds, and merge them into a
/// single `Filter` above the rebuilt `Join`. Conjuncts that fail the
/// binding gate stay on their arm (see the module doc for why). Every
/// other node passes its (already-recursed) children through unchanged —
/// in particular a `Filter` on the optional side of a `LeftJoin`, or under
/// a `Union`/`Distinct`/`Group`/`Slice`, is never touched, because
/// `pullup` only special-cases `Join`.
fn pullup(plan: LogicalPlan) -> LogicalPlan {
    let plan = map_children(plan, &pullup);
    match plan {
        LogicalPlan::Join { left, right } => {
            let (left, mut hoisted) = split_arm(*left);
            let (right, hoisted_from_right) = split_arm(*right);
            hoisted.extend(hoisted_from_right);

            let join = LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
            };
            match conjoin(hoisted) {
                Some(expr) => LogicalPlan::Filter {
                    expr,
                    inner: Box::new(join),
                },
                None => join,
            }
        }
        other => other,
    }
}

/// Peel the `Filter` chain off one join arm and split its conjuncts into
/// (rebuilt arm, hoistable conjuncts). A conjunct is hoistable only when
/// every variable it references is in [`bound_vars`] of the unwrapped arm
/// (legality is against the unwrapped subtree for every conjunct in the
/// chain — fine, since `Filter` binds nothing). Non-hoistable conjuncts
/// are rebuilt as a residual `Filter` on the arm, in their original order.
fn split_arm(arm: LogicalPlan) -> (LogicalPlan, Vec<Expr>) {
    let (inner, parts) = strip_filter(arm);
    if parts.is_empty() {
        return (inner, parts);
    }
    let bound = bound_vars(&inner);
    let (hoist, residual): (Vec<Expr>, Vec<Expr>) = parts.into_iter().partition(|e| {
        let mut refs = HashSet::new();
        referenced_vars(e, &mut refs);
        refs.iter().all(|v| bound.contains(v))
    });
    let arm = match conjoin(residual) {
        Some(expr) => LogicalPlan::Filter {
            expr,
            inner: Box::new(inner),
        },
        None => inner,
    };
    (arm, hoist)
}

/// Peel a chain of immediate `Filter` wrappers off `node`, collecting their
/// conjuncts (via [`conjuncts`], so a top-level `And` inside any one
/// `Filter` also splits). Stops at the first non-`Filter` node.
fn strip_filter(node: LogicalPlan) -> (LogicalPlan, Vec<Expr>) {
    let mut parts = Vec::new();
    let mut node = node;
    while let LogicalPlan::Filter { expr, inner } = node {
        conjuncts(expr, &mut parts);
        node = *inner;
    }
    (node, parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Expr, Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};

    fn var(n: &str) -> Term {
        Term::Var(Var::new(n))
    }
    fn ctx() -> PlanCtx {
        PlanCtx::default()
    }
    fn bgp(p: &str) -> LogicalPlan {
        LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri(format!("http://ex/{p}")),
                object: var(p),
            }],
        }
    }
    fn pred(v: &str) -> Expr {
        Expr::Gt(
            Box::new(Expr::Term(var(v))),
            Box::new(Expr::Term(Term::Literal("\"0\"".into()))),
        )
    }

    /// A Filter on the left arm of a Join is pulled above the Join.
    #[test]
    fn pulls_filter_above_join() {
        let left = LogicalPlan::Filter {
            expr: pred("a"),
            inner: Box::new(bgp("a")),
        };
        let plan = LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(bgp("b")),
        };
        let out = FilterPullup.run(plan, &ctx());
        assert!(
            matches!(&out, LogicalPlan::Filter { inner, .. } if matches!(**inner, LogicalPlan::Join { .. })),
            "filter must sit above the join; got {out:?}"
        );
    }

    /// Filters from both arms merge into one Filter (single conjunction).
    #[test]
    fn merges_both_arms_into_one_filter() {
        let left = LogicalPlan::Filter {
            expr: pred("a"),
            inner: Box::new(bgp("a")),
        };
        let right = LogicalPlan::Filter {
            expr: pred("b"),
            inner: Box::new(bgp("b")),
        };
        let plan = LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(right),
        };
        let out = FilterPullup.run(plan, &ctx());
        let LogicalPlan::Filter { expr, inner } = out else {
            panic!("expected one Filter, got {out:?}")
        };
        assert!(matches!(*inner, LogicalPlan::Join { .. }));
        let mut parts = Vec::new();
        crate::plan::passes::conjuncts(expr, &mut parts);
        assert_eq!(parts.len(), 2, "both arm filters must be conjoined");
    }

    /// A Filter over a variable its OWN arm never binds must NOT be hoisted.
    /// In that arm the filter evaluates error→false and drops every row; the
    /// other arm binds the variable, so hoisting above the join would let
    /// rows pass that the correct SPARQL answer excludes.
    #[test]
    fn never_hoists_conjunct_unbound_in_its_arm() {
        // Left arm binds ?s/?a only; the filter references ?b, bound only by
        // the right arm.
        let left = LogicalPlan::Filter {
            expr: pred("b"),
            inner: Box::new(bgp("a")),
        };
        let plan = LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(bgp("b")),
        };
        let out = FilterPullup.run(plan, &ctx());
        let LogicalPlan::Join { left, .. } = out else {
            panic!("filter must not be hoisted above the join; got {out:?}")
        };
        assert!(
            matches!(*left, LogicalPlan::Filter { .. }),
            "filter must stay on its own arm; got {left:?}"
        );
    }

    /// A Filter on the optional (right) arm of a LeftJoin must NOT be pulled
    /// up (semantics differ).
    #[test]
    fn never_pulls_across_leftjoin() {
        let right = LogicalPlan::Filter {
            expr: pred("b"),
            inner: Box::new(bgp("b")),
        };
        let plan = LogicalPlan::LeftJoin {
            left: Box::new(bgp("a")),
            right: Box::new(right),
            expr: None,
        };
        let out = FilterPullup.run(plan, &ctx());
        assert!(
            matches!(out, LogicalPlan::LeftJoin { .. }),
            "no hoist across LeftJoin; got {out:?}"
        );
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(FilterPullup.id(), PassId::FilterPullup);
        assert_eq!(FilterPullup.must_follow(), &[PassId::Normalize]);
    }
}
