//! `FilterPushdown` (SPEC-23 §5.2): push each conjunct to the deepest
//! subtree that binds all its variables. Legality is lattice-gated
//! (`bound_vars`); the `LeftJoin` asymmetry is honored — a conjunct never
//! descends into the optional (right) arm.
//!
//! Runs after [`crate::plan::passes::FilterPullup`], which hoists filters
//! above inner joins so every conjunct here sees the widest legal join it
//! could sink through. This pass only *moves* conjuncts to a legal deeper
//! point (or leaves them where they are); it never drops or changes one —
//! that's [`crate::plan::passes::Normalize`]'s job. `Bgp` is the deepest
//! sink: wrapping it directly is itself the one-level descent from the
//! caller's `Filter`, so a conjunct legal anywhere always finds a home.
//!
//! Conjuncts that land at the same tree position merge into one `Filter`
//! instead of nesting — rationale at [`push_one`]'s `Filter` arm.

use crate::algebra::Expr;
use crate::exec::runtime::referenced_vars;
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{bound_vars, conjoin, conjuncts, map_children};
use std::collections::HashSet;

pub struct FilterPushdown;

impl LogicalPass for FilterPushdown {
    fn id(&self) -> PassId {
        PassId::FilterPushdown
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        pushdown(plan)
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::FilterPullup]
    }
}

/// Bottom-up: push down every child first (so a conjunct sinks past any
/// `Filter` nested in a child before this node's own `Filter` is
/// considered), then split this node's `Filter` (if any) into conjuncts and
/// sink each one independently.
fn pushdown(plan: LogicalPlan) -> LogicalPlan {
    let plan = map_children(plan, &pushdown);
    match plan {
        LogicalPlan::Filter { expr, inner } => push_filter(expr, *inner),
        other => other,
    }
}

/// Split `expr` into conjuncts and sink each one into `inner` independently
/// — a later conjunct sees the tree as reshaped by earlier ones. Conjuncts
/// that reach no legal sink stay behind as a residual `Filter` on `inner`,
/// in their original order.
fn push_filter(expr: Expr, inner: LogicalPlan) -> LogicalPlan {
    let mut parts = Vec::new();
    conjuncts(expr, &mut parts);

    let mut cur = inner;
    let mut residual = Vec::new();
    for c in parts {
        match push_one(c, cur) {
            Ok(sunk) => cur = sunk,
            Err((c, unchanged)) => {
                residual.push(c);
                cur = unchanged;
            }
        }
    }
    match conjoin(residual) {
        Some(expr) => LogicalPlan::Filter {
            expr,
            inner: Box::new(cur),
        },
        None => cur,
    }
}

/// Try to sink `c` one level into `node`. `Ok` means `c` found a legal home
/// somewhere at or below `node` (via [`wrap`]'s recursion) and `node` was
/// rebuilt around it; `Err` returns `c` and `node` unchanged because `node`
/// is not a safe sink target (or none of its children bind `c`'s variables).
///
/// Legality re-runs [`bound_vars`] (a full lattice `infer`) on the candidate
/// arm at every descent level — accepted cost at planning time, on
/// planning-sized trees.
fn push_one(c: Expr, node: LogicalPlan) -> Result<LogicalPlan, (Expr, LogicalPlan)> {
    let mut refs = HashSet::new();
    referenced_vars(&c, &mut refs);
    let legal = |arm: &LogicalPlan| {
        let bound = bound_vars(arm);
        refs.iter().all(|v| bound.contains(v))
    };

    match node {
        LogicalPlan::Join { left, right } => {
            // Prefer the left arm when both qualify (matches the source
            // order `map_children`/`schema` already treat as canonical).
            if legal(&left) {
                Ok(LogicalPlan::Join {
                    left: Box::new(wrap(c, *left)),
                    right,
                })
            } else if legal(&right) {
                Ok(LogicalPlan::Join {
                    left,
                    right: Box::new(wrap(c, *right)),
                })
            } else {
                Err((c, LogicalPlan::Join { left, right }))
            }
        }
        // Only the mandatory (left) arm of a LeftJoin is a legal sink: a
        // conjunct that reached the right (optional) arm would filter out
        // the very rows OPTIONAL exists to keep unbound.
        LogicalPlan::LeftJoin { left, right, expr } => {
            if legal(&left) {
                Ok(LogicalPlan::LeftJoin {
                    left: Box::new(wrap(c, *left)),
                    right,
                    expr,
                })
            } else {
                Err((c, LogicalPlan::LeftJoin { left, right, expr }))
            }
        }
        // A Project is a SCOPE boundary, not just a column restriction: a
        // conjunct above it may legally reference a projected-away var —
        // out of scope, so it evaluates unbound → error → row dropped.
        // Pushing such a conjunct through would un-hide the var (the inner
        // binds it) and change results. So legality is gated on the Project
        // NODE's own visible output ([`bound_vars`] of the whole node —
        // `infer` already hides inner bindings for Project), never on its
        // inner. A conjunct that passes this gate references only projected
        // vars, and every projected var is by construction bound by the
        // inner, so descending via [`wrap`] is then safe.
        node @ LogicalPlan::Project { .. } => {
            if legal(&node) {
                let LogicalPlan::Project { vars, inner } = node else {
                    unreachable!("bound above as Project");
                };
                Ok(LogicalPlan::Project {
                    vars,
                    inner: Box::new(wrap(c, *inner)),
                })
            } else {
                Err((c, node))
            }
        }
        // The deepest possible sink: wrapping the leaf directly IS the
        // one-level descent the caller asked for, so this arm always
        // succeeds.
        LogicalPlan::Bgp { patterns } => Ok(LogicalPlan::Filter {
            expr: c,
            inner: Box::new(LogicalPlan::Bgp { patterns }),
        }),
        // A `Filter` already sitting here — either a `FilterPullup` residual
        // or one this same push_filter call created a moment ago by wrapping
        // a sibling conjunct at this exact spot — is not a boundary: `Filter`
        // binds no vars, and `Filter(P, a && b)` is exactly `Filter(Filter(P,
        // a), b)` for any boolean `a`, `b`. So merging `c` into the existing
        // conjunct set is a pure structural simplification, not a new
        // legality question (no need to re-check `bound_vars`), and it keeps
        // conjuncts that land at the same node in ONE `Filter` instead of a
        // nest of them — which matters downstream: `pushdown.rs`'s
        // count-pushdown pattern match (`lower_count_group`) only recognizes
        // `Filter { expr, inner: BgpScan }`, one layer deep.
        LogicalPlan::Filter { expr, inner } => {
            let mut parts = Vec::new();
            conjuncts(expr, &mut parts);
            parts.push(c);
            let merged = conjoin(parts).expect("parts has >= 1 element (c, just pushed)");
            Ok(LogicalPlan::Filter {
                expr: merged,
                inner,
            })
        }
        // Every other node is a hard boundary: `Union` (each branch has an
        // independent schema), `Distinct`/`Group`/`Slice`/`OrderBy` (a
        // predicate below them can change which/how-many rows survive
        // *before* the operator, silently changing its result), and
        // `Extend`/`Values`/`PathClosure` (no useful subtree to sink into).
        other => Err((c, other)),
    }
}

/// Sink `c` into `node` as deep as legally possible: recurse via
/// [`push_one`] until it returns `Err`, then wrap a `Filter` at that point.
fn wrap(c: Expr, node: LogicalPlan) -> LogicalPlan {
    match push_one(c, node) {
        Ok(sunk) => sunk,
        Err((c, unchanged)) => LogicalPlan::Filter {
            expr: c,
            inner: Box::new(unchanged),
        },
    }
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
    fn scan(subj: &str, p: &str, obj: &str) -> LogicalPlan {
        LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var(subj),
                predicate: Term::Iri(format!("http://ex/{p}")),
                object: var(obj),
            }],
        }
    }
    fn gt0(v: &str) -> Expr {
        Expr::Gt(
            Box::new(Expr::Term(var(v))),
            Box::new(Expr::Term(Term::Literal("\"0\"".into()))),
        )
    }

    /// A conjunct that mentions only left-arm vars pushes onto the left arm.
    #[test]
    fn pushes_single_var_conjunct_to_binding_arm() {
        let join = LogicalPlan::Join {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
        };
        let plan = LogicalPlan::Filter {
            expr: gt0("x"),
            inner: Box::new(join),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Join { left, right } = out else {
            panic!("expected Join at root, got {out:?}")
        };
        assert!(
            matches!(*left, LogicalPlan::Filter { .. }),
            "conjunct must push to the left arm; got {left:?}"
        );
        assert!(
            matches!(*right, LogicalPlan::Bgp { .. }),
            "right arm unfiltered; got {right:?}"
        );
    }

    /// A conjunct referencing a var bound only on the OPTIONAL side of a
    /// LeftJoin stays ABOVE the LeftJoin — never pushed into the right arm.
    #[test]
    fn respects_leftjoin_asymmetry() {
        let lj = LogicalPlan::LeftJoin {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
            expr: None,
        };
        let plan = LogicalPlan::Filter {
            expr: gt0("y"),
            inner: Box::new(lj),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Filter { inner, .. } = out else {
            panic!("filter must stay above LeftJoin, got {out:?}")
        };
        assert!(matches!(*inner, LogicalPlan::LeftJoin { .. }));
    }

    /// A conjunct on a var bound on the MANDATORY side of a LeftJoin DOES
    /// push into the left arm.
    #[test]
    fn pushes_into_leftjoin_mandatory_arm() {
        let lj = LogicalPlan::LeftJoin {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
            expr: None,
        };
        let plan = LogicalPlan::Filter {
            expr: gt0("x"),
            inner: Box::new(lj),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::LeftJoin { left, .. } = out else {
            panic!("expected LeftJoin root, got {out:?}")
        };
        assert!(
            matches!(*left, LogicalPlan::Filter { .. }),
            "mandatory-arm conjunct must push; got {left:?}"
        );
    }

    /// A conjunct spanning BOTH Join arms (`?x` left-only, `?y` right-only)
    /// has no single-arm home — it stays as a residual Filter above the Join.
    #[test]
    fn cross_arm_conjunct_stays_above_join() {
        let join = LogicalPlan::Join {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
        };
        let plan = LogicalPlan::Filter {
            expr: Expr::Gt(
                Box::new(Expr::Term(var("x"))),
                Box::new(Expr::Term(var("y"))),
            ),
            inner: Box::new(join),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Filter { inner, .. } = out else {
            panic!("cross-arm conjunct must stay above the Join, got {out:?}")
        };
        let LogicalPlan::Join { left, right } = *inner else {
            panic!("expected Join under the residual Filter, got {inner:?}")
        };
        assert!(matches!(*left, LogicalPlan::Bgp { .. }), "got {left:?}");
        assert!(matches!(*right, LogicalPlan::Bgp { .. }), "got {right:?}");
    }

    /// Two conjuncts over the same left-arm var: the first push wraps a
    /// Filter around the left arm's Bgp; the second must merge into that
    /// Filter mid-tree (the join-arm path, not the Filter-at-root case
    /// `merges_conjuncts_landing_on_the_same_sink` covers).
    #[test]
    fn merges_into_filter_placed_on_a_join_arm() {
        let join = LogicalPlan::Join {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
        };
        let plan = LogicalPlan::Filter {
            expr: Expr::And(Box::new(gt0("x")), Box::new(gt0("a"))),
            inner: Box::new(join),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Join { left, .. } = out else {
            panic!("both conjuncts must sink; expected Join at root, got {out:?}")
        };
        let LogicalPlan::Filter { expr, inner } = *left else {
            panic!("expected one merged Filter on the left arm, got {left:?}")
        };
        assert!(matches!(*inner, LogicalPlan::Bgp { .. }));
        let mut parts = Vec::new();
        crate::plan::passes::conjuncts(expr, &mut parts);
        assert_eq!(parts.len(), 2, "conjuncts must merge, not nest");
    }

    /// A conjunct over a var the `Project` hides must stay ABOVE the
    /// Project. `?y` is out of scope there — the filter evaluates unbound →
    /// error → drops every row. Pushing it inside would un-hide `?y` and
    /// change results (e.g. `SELECT ?x { { SELECT ?x { ?x :p ?y } }
    /// FILTER(?y > 0) }` must return 0 rows, not the rows where the hidden
    /// `?y` happens to be positive).
    #[test]
    fn never_pushes_through_project_scope_hiding() {
        let plan = LogicalPlan::Filter {
            expr: gt0("y"),
            inner: Box::new(LogicalPlan::Project {
                vars: vec![Var::new("x")],
                inner: Box::new(scan("x", "p1", "y")),
            }),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Filter { inner, .. } = out else {
            panic!("filter over a projected-away var must stay above the Project, got {out:?}")
        };
        assert!(
            matches!(&*inner, LogicalPlan::Project { inner, .. }
                if matches!(**inner, LogicalPlan::Bgp { .. })),
            "Project's inner must stay unfiltered; got {inner:?}"
        );
    }

    /// A conjunct over a var the `Project` keeps DOES push through, down to
    /// the `Bgp` underneath.
    #[test]
    fn pushes_through_project_over_visible_var() {
        let plan = LogicalPlan::Filter {
            expr: gt0("x"),
            inner: Box::new(LogicalPlan::Project {
                vars: vec![Var::new("x")],
                inner: Box::new(scan("x", "p1", "y")),
            }),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Project { inner, .. } = out else {
            panic!("expected Project at root, got {out:?}")
        };
        assert!(
            matches!(&*inner, LogicalPlan::Filter { inner, .. }
                if matches!(**inner, LogicalPlan::Bgp { .. })),
            "visible-var conjunct must push through onto the Bgp; got {inner:?}"
        );
    }

    /// Two conjuncts that both belong on the same `Bgp` land in ONE `Filter`
    /// with an `And`, not a nest of two `Filter`s. Regression coverage for a
    /// shape that broke `pushdown.rs`'s count-pushdown pattern match (which
    /// only recognizes `Filter { expr, inner: BgpScan }` one layer deep).
    #[test]
    fn merges_conjuncts_landing_on_the_same_sink() {
        let plan = LogicalPlan::Filter {
            expr: Expr::And(Box::new(gt0("x")), Box::new(gt0("a"))),
            inner: Box::new(scan("a", "p1", "x")),
        };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::Filter { expr, inner } = out else {
            panic!("expected a single Filter over the Bgp, got {out:?}")
        };
        assert!(matches!(*inner, LogicalPlan::Bgp { .. }));
        let mut parts = Vec::new();
        crate::plan::passes::conjuncts(expr, &mut parts);
        assert_eq!(
            parts.len(),
            2,
            "both conjuncts must merge into one Filter, not nest"
        );
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(FilterPushdown.id(), PassId::FilterPushdown);
        assert_eq!(FilterPushdown.must_follow(), &[PassId::FilterPullup]);
    }
}
