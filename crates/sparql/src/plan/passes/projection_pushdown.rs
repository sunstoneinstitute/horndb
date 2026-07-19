//! `ProjectionPushdown` (SPEC-23 §5.2): thread a demanded-variable set
//! top-down and insert restricting `Project` nodes so each subtree binds
//! only the variables an ancestor actually reads.
//!
//! This is the LOGICAL-plan mirror of [`crate::plan::pushdown::prune`], the
//! physical-plan pass that has done the same rewrite since Phase 1. Running
//! both is safe: each narrows independently at its own IR layer, both are
//! idempotent (a plan already narrowed to `demanded` re-narrows to the same
//! shape), and neither changes results — a restricting `Project`'s `kept`
//! set is always a subset of the wrapped node's own schema, so it can never
//! drop a variable any ancestor still reads. Retiring the physical pass once
//! this one is proven out is deferred to Phase 4.
//!
//! ## Soundness notes (subtleties this pass must get right)
//!
//! * **A `Project` may list vars its child never binds** ("projects
//!   unbound" — legal SPARQL). Restricting a child to a set of demanded
//!   names never disturbs this: [`restrict`] intersects `demanded` with the
//!   node's own *natural* schema, so a var absent from that schema is never
//!   added — it just stays unbound, exactly as before.
//! * **`Distinct` is a barrier.** It dedups whole rows, so narrowing its
//!   child would shrink the dedup key set and change the row count. Its
//!   child is pruned against its own *full* natural schema.
//! * **`Group` is usually a natural restriction point** (`keys ∪` the
//!   aggregates' input vars), **except** when an aggregate is
//!   `distinct && CountStar` — `COUNT(DISTINCT *)` dedups whole solution
//!   rows (its `agg_inner_exprs` is empty, which is exactly why that case
//!   needs an explicit check), so it becomes a `Distinct`-style full
//!   barrier.
//! * **`OrderBy` / `Filter` / `Extend` / `LeftJoin`'s ON expression** read
//!   variables they do not themselves output — "evaluate wide, project
//!   narrow": the child's demand is `demanded ∪ (the operator's own expr
//!   vars)`, and the rebuilt operator is passed through [`restrict`]
//!   afterward in case that widening leaves its output broader than the
//!   parent asked for.
//! * **`Union` arms may have different schemas.** This pass recurses into
//!   both arms with the *same* `demanded` set and never wraps the `Union`
//!   node itself. Sound even though the arms diverge: [`restrict`] only
//!   keeps vars in the wrapped node's own natural schema, so a demanded
//!   variable an arm never binds is never a candidate inside that arm —
//!   nothing is dropped that wasn't already absent. And a restricting
//!   `Project` inserted inside one arm can only remove variables that arm
//!   itself binds, which says nothing about the other arm's rows.
//! * **`PathClosure`** stays conservative: the closure machinery
//!   (transitive/BFS evaluation) reads the edge subplan's endpoint
//!   variables in ways this pass does not model, so the edge is pruned
//!   against its own full natural schema, never narrower.
//!
//! The debug dangling-refs validator in `run_passes` only flags *new*
//! dangling references. Inserting a restricting `Project` cannot introduce
//! one: [`restrict`]'s `kept` set is always a subset of the wrapped node's
//! own schema, so every kept var is already bound there.

use crate::algebra::{AggFunc, Var};
use crate::exec::runtime::{agg_inner_exprs, referenced_vars};
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::schema;
use std::collections::HashSet;

pub struct ProjectionPushdown;

impl LogicalPass for ProjectionPushdown {
    fn id(&self) -> PassId {
        PassId::ProjectionPushdown
    }
    /// The root demands its own full natural output — it is never narrowed
    /// below what the un-rewritten plan already emits — then [`prune`]
    /// threads that demand down.
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        let demanded = set_of(&schema(&plan));
        prune(plan, &demanded)
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::FilterPushdown]
    }
}

fn set_of(vars: &[Var]) -> HashSet<String> {
    vars.iter().map(|v| v.name().to_owned()).collect()
}

fn intersect(demanded: &HashSet<String>, scope: &[Var]) -> HashSet<String> {
    scope
        .iter()
        .filter(|v| demanded.contains(v.name()))
        .map(|v| v.name().to_owned())
        .collect()
}

/// The vars two join arms share (present in both `lo` and `ro`), added to
/// `demanded` — a join key an ancestor never reads is still required so the
/// join itself can match rows across arms.
fn add_shared_keys(demanded: &HashSet<String>, lo: &[Var], ro: &[Var]) -> HashSet<String> {
    let ro_names: HashSet<&str> = ro.iter().map(|v| v.name()).collect();
    let mut base = demanded.clone();
    for v in lo {
        if ro_names.contains(v.name()) {
            base.insert(v.name().to_owned());
        }
    }
    base
}

/// Wrap `node` in a restricting `Project` down to `node`'s own schema ∩
/// `demanded`, in schema order — but only when that actually narrows
/// something (`kept` shorter than the natural schema) and leaves at least
/// one variable (an empty `Project` is not a useful narrowing to emit; the
/// operator above already gates on row count, not columns).
fn restrict(node: LogicalPlan, demanded: &HashSet<String>) -> LogicalPlan {
    let natural = schema(&node);
    let kept: Vec<Var> = natural
        .iter()
        .filter(|v| demanded.contains(v.name()))
        .cloned()
        .collect();
    if kept.len() < natural.len() && !kept.is_empty() {
        LogicalPlan::Project {
            vars: kept,
            inner: Box::new(node),
        }
    } else {
        node
    }
}

/// Rewrite `node` so its output still covers `demanded ∩ (node's producible
/// vars)`, inserting restricting `Project`s on every edge that can be
/// safely narrowed. Mirrors `plan::pushdown::prune`'s per-variant logic —
/// see the module doc for the soundness reasoning behind each case.
fn prune(node: LogicalPlan, demanded: &HashSet<String>) -> LogicalPlan {
    use LogicalPlan::*;
    match node {
        leaf @ (Bgp { .. } | Values { .. }) => restrict(leaf, demanded),
        // The Project itself IS the restriction point: it always outputs
        // exactly `vars`, so its own var list — not the incoming
        // `demanded` — becomes the child's demand. This can nest a
        // restricting `Project` (from [`restrict`], further down) directly
        // under this one. Deliberately not collapsed: `inner` may be a
        // genuine SPARQL subquery `Project` (a scope boundary) whose own
        // `vars` are independent of `want` and may hide variables its
        // child still binds; there is no way to tell that apart from a
        // restrict-inserted `Project` once both have this shape, so this
        // arm never unwraps one.
        Project { vars, inner } => {
            let want = set_of(&vars);
            LogicalPlan::Project {
                vars,
                inner: Box::new(prune(*inner, &want)),
            }
        }
        Filter { expr, inner } => {
            let mut d = demanded.clone();
            referenced_vars(&expr, &mut d);
            let pi = prune(*inner, &d);
            let node2 = LogicalPlan::Filter {
                expr,
                inner: Box::new(pi),
            };
            restrict(node2, demanded)
        }
        Join { left, right } => {
            let lo = schema(&left);
            let ro = schema(&right);
            let base = add_shared_keys(demanded, &lo, &ro);
            let pl = prune(*left, &intersect(&base, &lo));
            let pr = prune(*right, &intersect(&base, &ro));
            let node2 = LogicalPlan::Join {
                left: Box::new(pl),
                right: Box::new(pr),
            };
            restrict(node2, demanded)
        }
        LeftJoin { left, right, expr } => {
            let lo = schema(&left);
            let ro = schema(&right);
            let mut base = add_shared_keys(demanded, &lo, &ro);
            if let Some(e) = &expr {
                referenced_vars(e, &mut base);
            }
            let pl = prune(*left, &intersect(&base, &lo));
            let pr = prune(*right, &intersect(&base, &ro));
            let node2 = LogicalPlan::LeftJoin {
                left: Box::new(pl),
                right: Box::new(pr),
                expr,
            };
            restrict(node2, demanded)
        }
        // See module doc: never wrap the Union itself, recurse into both
        // arms with the same `demanded` set.
        Union { left, right } => LogicalPlan::Union {
            left: Box::new(prune(*left, demanded)),
            right: Box::new(prune(*right, demanded)),
        },
        // Barrier: dedups on the child's full natural schema.
        Distinct { inner } => {
            let nat = set_of(&schema(&inner));
            LogicalPlan::Distinct {
                inner: Box::new(prune(*inner, &nat)),
            }
        }
        Slice {
            inner,
            start,
            length,
        } => LogicalPlan::Slice {
            inner: Box::new(prune(*inner, demanded)),
            start,
            length,
        },
        OrderBy { inner, keys } => {
            let mut d = demanded.clone();
            for (e, _) in &keys {
                referenced_vars(e, &mut d);
            }
            let pi = prune(*inner, &d);
            let node2 = LogicalPlan::OrderBy {
                inner: Box::new(pi),
                keys,
            };
            restrict(node2, demanded)
        }
        Extend { inner, var, expr } => {
            let mut d = demanded.clone();
            d.remove(var.name());
            referenced_vars(&expr, &mut d);
            let pi = prune(*inner, &d);
            let node2 = LogicalPlan::Extend {
                inner: Box::new(pi),
                var,
                expr,
            };
            restrict(node2, demanded)
        }
        // Whole-row dedup barrier only for `distinct && CountStar` — see
        // module doc. Otherwise the child demand is `keys ∪` the
        // aggregates' input vars.
        Group {
            inner,
            keys,
            aggregates,
        } => {
            let distinct_star = aggregates
                .iter()
                .any(|a| matches!(a.func, AggFunc::CountStar) && a.distinct);
            let d = if distinct_star {
                set_of(&schema(&inner))
            } else {
                let mut d = set_of(&keys);
                for a in &aggregates {
                    for e in agg_inner_exprs(a) {
                        referenced_vars(e, &mut d);
                    }
                }
                d
            };
            LogicalPlan::Group {
                inner: Box::new(prune(*inner, &d)),
                keys,
                aggregates,
            }
        }
        PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => {
            let nat = set_of(&schema(&edge));
            LogicalPlan::PathClosure {
                subject,
                object,
                edge: Box::new(prune(*edge, &nat)),
                reflexive,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
    use crate::plan::passes::schema;

    fn var(n: &str) -> Term {
        Term::Var(Var::new(n))
    }
    fn ctx() -> PlanCtx {
        PlanCtx::default()
    }

    /// `Project([?s], Bgp(?s ?p ?o))` narrows the scan so only ?s survives
    /// below the Project (a restricting Project wraps the scan).
    #[test]
    fn narrows_scan_under_project() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let plan = LogicalPlan::Project {
            vars: vec![Var::new("s")],
            inner: Box::new(bgp),
        };
        let out = ProjectionPushdown.run(plan, &ctx());
        let LogicalPlan::Project { inner, .. } = out else {
            panic!("expected Project root")
        };
        let sch = schema(&inner);
        assert_eq!(
            sch,
            vec![Var::new("s")],
            "scan must be narrowed to ?s; got {sch:?}"
        );
    }

    /// DISTINCT is a barrier: its child keeps its full natural schema (else
    /// the dedup key set changes).
    #[test]
    fn distinct_is_a_barrier() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let plan = LogicalPlan::Project {
            vars: vec![Var::new("s")],
            inner: Box::new(LogicalPlan::Distinct {
                inner: Box::new(bgp),
            }),
        };
        let out = ProjectionPushdown.run(plan, &ctx());
        fn distinct_child(p: &LogicalPlan) -> Option<&LogicalPlan> {
            match p {
                LogicalPlan::Distinct { inner } => Some(inner),
                LogicalPlan::Project { inner, .. } => distinct_child(inner),
                _ => None,
            }
        }
        let child = distinct_child(&out).expect("Distinct preserved");
        let mut sch = schema(child);
        sch.sort_by(|a, b| a.name().cmp(b.name()));
        assert_eq!(
            sch,
            vec![Var::new("o"), Var::new("p"), Var::new("s")],
            "Distinct child must keep full dedup key; got {sch:?}"
        );
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(ProjectionPushdown.id(), PassId::ProjectionPushdown);
        assert_eq!(ProjectionPushdown.must_follow(), &[PassId::FilterPushdown]);
    }
}
