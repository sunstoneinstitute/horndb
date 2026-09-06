//! Algebra ⇄ physical bridging through the logical IR (SPEC-23 §5.1).
//!
//! [`lower_algebra`] is a **naive** 1:1 image of `crate::algebra::Algebra`
//! into [`LogicalPlan`] — no coalescing, no folding — so that
//! `lower_physical(lower_algebra(alg))` is structurally identical to the
//! pre-refactor `planner::plan(alg)`. Coalescing is a *pass* (`CoalesceBgp`),
//! keeping the transformation in one bisectable place. [`lower_physical`]
//! maps a (possibly coalesced) [`LogicalPlan`] back to
//! [`crate::plan::PhysicalPlan`]; a flat `Bgp` lowers to a `BgpScan` with
//! the same patterns and graph scope, which the WCOJ executor runs as the
//! natural join of the whole pattern set — result-equivalent to the nested
//! `Join(BgpScan, BgpScan)` today's lowering emits (proven in
//! `tests/logical_pipeline.rs`).
//!
//! Lowering is also where the `GRAPH` scope lands. Ground `GRAPH <g>` is not
//! kept as a node — it sets the scope on every scan leaf beneath it
//! (SPEC-28 S3/D5). `GRAPH ?g` becomes one [`LogicalPlan::PerGraph`] node
//! over the same scoped leaves (SPEC-28 D6). See [`lower_scoped`].

use crate::algebra::{Algebra, GraphSpec};
use crate::error::Result;
use crate::plan::logical::LogicalPlan;
use crate::plan::{GraphScope, PhysicalPlan};

/// Naive `Algebra → LogicalPlan` (no coalescing, no folding), starting from
/// the query's default graph.
pub fn lower_algebra(alg: &Algebra) -> Result<LogicalPlan> {
    lower_scoped(alg, &GraphScope::DefaultGraph)
}

/// `lower_algebra`'s recursion, carrying the graph scope in force at this
/// point in the tree (SPEC-28 S3/D5).
///
/// `Algebra::Graph` replaces `scope` for its whole subtree, so every scan
/// leaf underneath is built already scoped. A nested `GRAPH` therefore
/// overrides the outer one — innermost wins, per SPARQL 1.1. The ground form
/// leaves no node behind; the variable form leaves one
/// [`LogicalPlan::PerGraph`], which binds `?g` on the block's rows after the
/// block has been evaluated with `?g` free (§18.2.2.2, SPEC-28 D6).
/// Every other operator passes `scope` through unchanged; that
/// includes `PathClosure`, whose `edge` sub-plan is scoped *before* the
/// closure is computed (S3: post-filtering would admit paths that leave the
/// graph and come back).
///
/// `Values` is scope-free by construction — its rows are literals, not
/// stored quads — so it needs no scope field.
fn lower_scoped(alg: &Algebra, scope: &GraphScope) -> Result<LogicalPlan> {
    Ok(match alg {
        Algebra::Bgp { patterns } => LogicalPlan::Bgp {
            patterns: patterns.clone(),
            scope: scope.clone(),
        },
        Algebra::Join { left, right } => LogicalPlan::Join {
            left: Box::new(lower_scoped(left, scope)?),
            right: Box::new(lower_scoped(right, scope)?),
        },
        Algebra::LeftJoin { left, right, expr } => LogicalPlan::LeftJoin {
            left: Box::new(lower_scoped(left, scope)?),
            right: Box::new(lower_scoped(right, scope)?),
            expr: expr.clone(),
        },
        Algebra::Minus { left, right } => LogicalPlan::Minus {
            left: Box::new(lower_scoped(left, scope)?),
            right: Box::new(lower_scoped(right, scope)?),
        },
        Algebra::Filter { expr, inner } => LogicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(lower_scoped(inner, scope)?),
        },
        Algebra::Union { left, right } => LogicalPlan::Union {
            left: Box::new(lower_scoped(left, scope)?),
            right: Box::new(lower_scoped(right, scope)?),
        },
        Algebra::Project { vars, inner } => LogicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(lower_scoped(inner, scope)?),
        },
        Algebra::Distinct { inner } => LogicalPlan::Distinct {
            inner: Box::new(lower_scoped(inner, scope)?),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => LogicalPlan::Slice {
            inner: Box::new(lower_scoped(inner, scope)?),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => LogicalPlan::OrderBy {
            inner: Box::new(lower_scoped(inner, scope)?),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => LogicalPlan::Extend {
            inner: Box::new(lower_scoped(inner, scope)?),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => LogicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => LogicalPlan::Group {
            inner: Box::new(lower_scoped(inner, scope)?),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => LogicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(lower_scoped(edge, scope)?),
            reflexive: *reflexive,
        },
        Algebra::Graph { name, inner } => {
            let scope = GraphScope::Named(name.clone());
            let lowered = lower_scoped(inner, &scope)?;
            match name {
                GraphSpec::Iri(_) => lowered,
                GraphSpec::Var(v) => LogicalPlan::PerGraph {
                    var: v.clone(),
                    inner: Box::new(lowered),
                },
            }
        }
    })
}

/// `LogicalPlan → PhysicalPlan`. A flat `Bgp` lowers to `BgpScan` (the WCOJ
/// executor runs the whole pattern set as one natural join). Takes the plan
/// by value: the pipeline hands over an owned `LogicalPlan`, so the fields
/// move instead of deep-cloning a second time (the algebra→logical lowering
/// already cloned once).
pub fn lower_physical(plan: LogicalPlan) -> PhysicalPlan {
    match plan {
        LogicalPlan::Bgp { patterns, scope } => PhysicalPlan::BgpScan { patterns, scope },
        LogicalPlan::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(lower_physical(*left)),
            right: Box::new(lower_physical(*right)),
        },
        LogicalPlan::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(lower_physical(*left)),
            right: Box::new(lower_physical(*right)),
            expr,
        },
        LogicalPlan::Minus { left, right } => PhysicalPlan::Minus {
            left: Box::new(lower_physical(*left)),
            right: Box::new(lower_physical(*right)),
        },
        LogicalPlan::Filter { expr, inner } => PhysicalPlan::Filter {
            expr,
            inner: Box::new(lower_physical(*inner)),
        },
        LogicalPlan::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(lower_physical(*left)),
            right: Box::new(lower_physical(*right)),
        },
        LogicalPlan::Project { vars, inner } => PhysicalPlan::Project {
            vars,
            inner: Box::new(lower_physical(*inner)),
        },
        LogicalPlan::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(lower_physical(*inner)),
        },
        LogicalPlan::Slice {
            inner,
            start,
            length,
        } => PhysicalPlan::Slice {
            inner: Box::new(lower_physical(*inner)),
            start,
            length,
        },
        LogicalPlan::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(lower_physical(*inner)),
            keys,
        },
        LogicalPlan::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(lower_physical(*inner)),
            var,
            expr,
        },
        LogicalPlan::Values { vars, rows } => PhysicalPlan::Values { vars, rows },
        LogicalPlan::Group {
            inner,
            keys,
            aggregates,
        } => PhysicalPlan::Group {
            inner: Box::new(lower_physical(*inner)),
            keys,
            aggregates,
        },
        LogicalPlan::PerGraph { var, inner } => PhysicalPlan::PerGraph {
            var,
            inner: Box::new(lower_physical(*inner)),
        },
        LogicalPlan::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PhysicalPlan::PathClosure {
            subject,
            object,
            edge: Box::new(lower_physical(*edge)),
            reflexive,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};

    fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
        TriplePattern {
            subject: Term::Var(Var::new(s)),
            predicate: Term::Iri(p.to_owned()),
            object: Term::Var(Var::new(o)),
        }
    }

    #[test]
    fn bgp_round_trips_to_bgp_scan() {
        let alg = Algebra::Bgp {
            patterns: vec![pat("s", "http://ex/p", "o")],
        };
        let phys = lower_physical(lower_algebra(&alg).unwrap());
        assert_eq!(
            phys,
            PhysicalPlan::BgpScan {
                patterns: vec![pat("s", "http://ex/p", "o")],
                scope: GraphScope::DefaultGraph,
            }
        );
    }

    /// `GRAPH` does not survive lowering: its scope lands on the scan leaf,
    /// and a nested `GRAPH` wins over the outer one (SPEC-28 S3).
    #[test]
    fn graph_scope_lands_on_the_scan_leaf_innermost_wins() {
        use crate::algebra::GraphSpec;
        let inner = Algebra::Bgp {
            patterns: vec![pat("s", "http://ex/p", "o")],
        };
        let alg = Algebra::Graph {
            name: GraphSpec::Iri("http://ex/outer".into()),
            inner: Box::new(Algebra::Graph {
                name: GraphSpec::Iri("http://ex/inner".into()),
                inner: Box::new(inner),
            }),
        };
        match lower_physical(lower_algebra(&alg).unwrap()) {
            PhysicalPlan::BgpScan { scope, .. } => assert_eq!(
                scope,
                GraphScope::Named(GraphSpec::Iri("http://ex/inner".into()))
            ),
            other => panic!("expected a scoped BgpScan, got {other:?}"),
        }
    }

    /// `GRAPH ?g` keeps one node — the per-graph loop — over a scan leaf
    /// scoped to the variable (SPEC-28 D6).
    #[test]
    fn graph_var_lowers_to_one_per_graph_node() {
        use crate::algebra::GraphSpec;
        let alg = Algebra::Graph {
            name: GraphSpec::Var(Var::new("g")),
            inner: Box::new(Algebra::Bgp {
                patterns: vec![pat("s", "http://ex/p", "o")],
            }),
        };
        match lower_physical(lower_algebra(&alg).unwrap()) {
            PhysicalPlan::PerGraph { var, inner } => {
                assert_eq!(var.name(), "g");
                match *inner {
                    PhysicalPlan::BgpScan { scope, .. } => {
                        assert_eq!(scope, GraphScope::Named(GraphSpec::Var(Var::new("g"))))
                    }
                    other => panic!("expected a scoped BgpScan, got {other:?}"),
                }
            }
            other => panic!("expected PerGraph, got {other:?}"),
        }
    }

    #[test]
    fn naive_join_stays_a_nested_join() {
        // lower_algebra must NOT coalesce — that is CoalesceBgp's job.
        let alg = Algebra::Join {
            left: Box::new(Algebra::Bgp {
                patterns: vec![pat("s", "http://ex/p", "o")],
            }),
            right: Box::new(Algebra::Bgp {
                patterns: vec![pat("o", "http://ex/q", "z")],
            }),
        };
        let log = lower_algebra(&alg).unwrap();
        assert!(
            matches!(log, LogicalPlan::Join { .. }),
            "naive lowering keeps the Join; got {log:?}"
        );
        assert!(matches!(lower_physical(log), PhysicalPlan::Join { .. }));
    }
}
