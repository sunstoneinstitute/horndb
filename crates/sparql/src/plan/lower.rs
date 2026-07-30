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
//! Lowering is also where the `GRAPH` scope lands: `Algebra::Graph` is not
//! kept as a node — it sets the scope on every scan leaf beneath it
//! (SPEC-28 S3/D5). See [`lower_scoped`].

use crate::algebra::{Algebra, Var};
use crate::error::{Result, SparqlError};
use crate::plan::logical::LogicalPlan;
use crate::plan::{GraphScope, PhysicalPlan};

/// Naive `Algebra → LogicalPlan` (no coalescing, no folding), starting from
/// the query's default graph.
///
/// Errors when a scope cannot be pushed all the way down — see
/// [`per_graph_barrier`]. That is by design: a scope that cannot be pushed must
/// fail here rather than fall back to a post-filter, which would answer a
/// different question (SPEC-28 D5).
pub fn lower_algebra(alg: &Algebra) -> Result<LogicalPlan> {
    lower_scoped(alg, &GraphScope::DefaultGraph)
}

/// `lower_algebra`'s recursion, carrying the graph scope in force at this
/// point in the tree (SPEC-28 S3/D5).
///
/// `Algebra::Graph` does not survive lowering: it replaces `scope` for its
/// whole subtree, so every scan leaf underneath is built already scoped.
/// A nested `GRAPH` therefore overrides the outer one — innermost wins, per
/// SPARQL 1.1. Every other operator passes `scope` through unchanged; that
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
            // `GRAPH ?g` binds `?g` as a column of each scan leaf, so the
            // whole subtree has to carry that column up. Refuse when it
            // cannot (SPEC-28 D1: no wrong answers).
            if let Some(var) = scope.graph_var() {
                if let Some(barrier) = per_graph_barrier(&lowered, var) {
                    return Err(SparqlError::UnsupportedAlgebra(format!(
                        "{barrier} inside GRAPH ?{g} is not supported yet (SPEC-28 \
                         S3, #266): it would drop or merge the graph column, so rows \
                         would come back mixed across graphs, or with ?{g} unbound; \
                         use a ground GRAPH <iri>, or move it outside the GRAPH block",
                        g = var.name()
                    )));
                }
            }
            lowered
        }
    })
}

/// What stops the plan under a `GRAPH ?g` from carrying the graph column to
/// the top, named for the error message — or `None` when nothing does.
///
/// The rule is deliberately conservative: every leaf that reads quads must be
/// a scan carrying *this* graph variable, and every node above it must pass
/// rows through one-for-one. A node that narrows columns (`Project` — i.e. a
/// sub-SELECT), merges rows (`Distinct`, `Group`), truncates them (`Slice`),
/// or rewrites the relation (`PathClosure`) loses the graph column or blends
/// graphs together before the result is built. Relaxing a case means teaching
/// the operator to iterate per graph (PLAN-28-03 Task 5 and later), never
/// dropping the check.
///
/// Join arms are the one place the rule relaxes: an arm that reads no quads
/// (`VALUES`, and anything built from it) binds nothing per-graph, and joining
/// it against a scoped arm keeps the graph column of that arm on every output
/// row. `Union` still needs **both** arms scoped — an unscoped arm would
/// contribute rows with `?g` unbound — and `LeftJoin` still needs its left
/// arm scoped, for the same reason.
///
/// Barrier nodes report the **innermost** barrier, because the translator
/// builds property paths out of these nodes (`translate.rs`: a path becomes
/// `Distinct(Project(…))`, or `Slice(…, 0, 1)` when both endpoints are
/// ground). Naming the outermost node would tell a user who wrote `:p+` that
/// `DISTINCT` is unsupported, which they never wrote — hence the labels that
/// name both possible sources.
fn per_graph_barrier(plan: &LogicalPlan, var: &Var) -> Option<&'static str> {
    use LogicalPlan::*;
    match plan {
        // The one shape that works: a scan leaf carrying this scope.
        Bgp { scope, .. } if scope.graph_var() == Some(var) => None,
        // A scan under a *different* scope means an inner `GRAPH` took over
        // (innermost wins), which leaves the outer `?g` unbound.
        Bgp { .. } => Some("a nested GRAPH"),
        Filter { inner, .. } | Extend { inner, .. } | OrderBy { inner, .. } => {
            per_graph_barrier(inner, var)
        }
        // A quad-free arm (`VALUES`) is fine on either side of a join: the
        // other arm's graph column reaches every joined row.
        Join { left, right } => match (per_graph_barrier(left, var), per_graph_barrier(right, var))
        {
            (None, None) => None,
            (None, Some(_)) if reads_no_quads(right) => None,
            (Some(_), None) if reads_no_quads(left) => None,
            (l, r) => l.or(r),
        },
        // The left arm must bind `?g` — an unmatched left row still has to
        // carry it. The right arm may be quad-free (an OPTIONAL VALUES).
        LeftJoin { left, right, .. } => per_graph_barrier(left, var).or_else(|| {
            (!reads_no_quads(right))
                .then(|| per_graph_barrier(right, var))
                .flatten()
        }),
        Union { left, right } => {
            per_graph_barrier(left, var).or_else(|| per_graph_barrier(right, var))
        }
        Project { inner, .. } => {
            per_graph_barrier(inner, var).or(Some("a sub-SELECT or a property path"))
        }
        Distinct { inner } => per_graph_barrier(inner, var).or(Some("DISTINCT")),
        Slice { inner, .. } => {
            per_graph_barrier(inner, var).or(Some("LIMIT/OFFSET or a property path"))
        }
        Group { inner, .. } => per_graph_barrier(inner, var).or(Some("an aggregate or GROUP BY")),
        PathClosure { edge, .. } => per_graph_barrier(edge, var).or(Some("a property path")),
        Values { .. } => Some("VALUES"),
    }
}

/// Debug-only postcondition: no node dropped a graph column that something
/// above it still expects.
///
/// [`per_graph_barrier`] runs while the `Algebra::Graph` node still exists.
/// Lowering then dissolves that node, so every later pass sees scoped scan
/// leaves with no record of where the checked subtree ended. Nothing but this
/// check would notice a pass inserting a `Project` between a
/// `BgpScan { scope: Named(Var(g)) }` and a consumer of `g` — and that is the
/// silent wrong answer this task exists to remove. Today the only
/// barrier-inserting sites are `passes::projection_pushdown::restrict` and
/// `pushdown::wrap_if_wider`, both of which build their column set from a
/// demand set that includes `g`; this makes that a checked property rather
/// than a reviewed one.
///
/// Flags a node `n` when a child `c` outputs `g`, `n` does not, and an
/// ancestor still does. Aggregating nodes are exempt: dropping non-key
/// columns is what they are for, and lowering has already guaranteed no
/// aggregate sits *inside* a `GRAPH ?g`.
///
/// Known residual: two independent variables that happen to share a name —
/// a sub-SELECT with `GRAPH ?g` inside that does not project `?g`, inside a
/// query that binds `?g` elsewhere — look identical here and would trip this.
/// If that ever fires, exempt the shape; do not weaken the rule.
#[cfg(debug_assertions)]
pub(crate) fn per_graph_columns_survive(plan: &PhysicalPlan) -> Result<()> {
    fn per_graph_vars(node: &PhysicalPlan, out: &mut Vec<String>) {
        if let PhysicalPlan::BgpScan { scope, .. }
        | PhysicalPlan::CountScan { scope, .. }
        | PhysicalPlan::GroupCountScan { scope, .. } = node
        {
            if let Some(g) = scope.graph_var() {
                out.push(g.name().to_owned());
            }
        }
        for child in crate::plan::explain::children(node) {
            per_graph_vars(child, out);
        }
    }

    fn aggregating(node: &PhysicalPlan) -> bool {
        matches!(
            node,
            PhysicalPlan::Group { .. }
                | PhysicalPlan::CountScan { .. }
                | PhysicalPlan::GroupCountScan { .. }
        )
    }

    fn walk(node: &PhysicalPlan, wanted: &[String]) -> Result<()> {
        let out = crate::plan::pushdown::output_vars(node);
        let mut wanted_below: Vec<String> = wanted.to_vec();
        for v in &out {
            if !wanted_below.contains(v) {
                wanted_below.push(v.clone());
            }
        }
        for child in crate::plan::explain::children(node) {
            if !aggregating(node) {
                let child_out = crate::plan::pushdown::output_vars(child);
                let mut scoped = Vec::new();
                per_graph_vars(child, &mut scoped);
                for g in scoped {
                    if child_out.contains(&g) && !out.contains(&g) && wanted.contains(&g) {
                        return Err(SparqlError::Planner(format!(
                            "graph column ?{g} is dropped by {} but still expected \
                             above it (SPEC-28 S3/D6) — a plan pass narrowed a \
                             GRAPH ?{g} scan's output",
                            crate::plan::explain::node_label(node)
                        )));
                    }
                }
            }
            walk(child, &wanted_below)?;
        }
        Ok(())
    }

    walk(plan, &crate::plan::pushdown::output_vars(plan))
}

/// True when `plan` reads no quads at all — i.e. holds no `Bgp`. Such a
/// subtree has nothing to scope, so it neither needs the graph column nor can
/// smuggle rows in from another graph.
fn reads_no_quads(plan: &LogicalPlan) -> bool {
    use LogicalPlan::*;
    match plan {
        Bgp { .. } => false,
        Values { .. } => true,
        Filter { inner, .. }
        | Extend { inner, .. }
        | OrderBy { inner, .. }
        | Project { inner, .. }
        | Distinct { inner }
        | Slice { inner, .. }
        | Group { inner, .. } => reads_no_quads(inner),
        PathClosure { edge, .. } => reads_no_quads(edge),
        Join { left, right } | LeftJoin { left, right, .. } | Union { left, right } => {
            reads_no_quads(left) && reads_no_quads(right)
        }
    }
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

    /// The debug tripwire must actually trip: a hand-built plan in which a
    /// narrowing `Project` sits between a `GRAPH ?g` scan and a consumer of
    /// `?g` is exactly what a future pass could produce, and exactly what
    /// nothing else would catch.
    #[cfg(debug_assertions)]
    #[test]
    fn postcondition_catches_a_narrowed_graph_column() {
        use crate::algebra::GraphSpec;
        let scan = PhysicalPlan::BgpScan {
            patterns: vec![pat("s", "http://ex/p", "o")],
            scope: GraphScope::Named(GraphSpec::Var(Var::new("g"))),
        };
        let project = |vars: &[&str], inner: PhysicalPlan| PhysicalPlan::Project {
            vars: vars.iter().map(|v| Var::new(*v)).collect(),
            inner: Box::new(inner),
        };
        // Healthy: the projection keeps ?g.
        assert!(per_graph_columns_survive(&project(&["g", "s"], scan.clone())).is_ok());
        // Broken: an inner projection drops ?g while the outer one still
        // asks for it — every row would carry an unbound ?g.
        let narrowed = project(&["g", "s"], project(&["s"], scan.clone()));
        let err = per_graph_columns_survive(&narrowed).expect_err("must flag the dropped column");
        assert!(err.to_string().contains("?g"), "{err}");
        // Not flagged: nothing above wants ?g (the sub-SELECT case).
        assert!(per_graph_columns_survive(&project(&["s"], scan)).is_ok());
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
