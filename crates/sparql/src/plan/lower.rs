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

use crate::algebra::{Algebra, Expr, Term, TriplePattern, Var};
use crate::error::{Result, SparqlError};
use crate::plan::logical::LogicalPlan;
use crate::plan::{GraphScope, PhysicalPlan};
use std::collections::HashSet;

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
                if let Some(divergence) = per_graph_var_divergence(&lowered, var) {
                    return Err(SparqlError::UnsupportedAlgebra(format!(
                        "{divergence} inside GRAPH ?{g} is not supported yet (SPEC-28 \
                         S3, #266): SPARQL 1.1 §18.2.2.2 evaluates the block with \
                         ?{g} still free and joins the graph name on afterwards, \
                         while HornDB binds ?{g} on the scan leaf — the two answer \
                         differently in exactly this position; use a ground \
                         GRAPH <iri>, or move it outside the GRAPH block",
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

/// Where the pattern's own use of `?g` makes leaf-binding differ from the
/// spec's post-join, named for the error message — or `None` when it does not.
///
/// SPARQL 1.1 §18.2.2.2 defines `GRAPH ?g { P }` as: evaluate `P` in each
/// named graph **with `?g` still free**, then join each result with
/// `{?g → thatGraph}`. HornDB fuses that join into the scan leaves (D5/D6).
/// The two agree only when `?g` arrives at the top of `P` through ordinary
/// inner-join compatibility on a value the *data* supplied — i.e. when `?g`
/// occurs only
///
/// * in triple-pattern positions of a scoped `Bgp`, or as a `VALUES`
///   variable, and
/// * is combined upward by `Join`, `Union`, or the **left** arm of a
///   `LeftJoin`.
///
/// Then "the leaf keeps rows whose `?g` equals this graph" *is* the post-join
/// (`graph-variable-join`, and `GRAPH ?g { ?g ?p ?o }`). Everywhere else they
/// diverge, and this returns the reason:
///
/// * **Expressions** (`FILTER`, `BIND`'s expression, an `OPTIONAL` condition,
///   `ORDER BY`, `GROUP BY`/aggregates): the spec evaluates them with `?g`
///   unbound — `GRAPH ?g { FILTER(BOUND(?g)) }` is 0 rows, not one row per
///   graph (W3C `graph-variable-scope`).
/// * **`BIND(… AS ?g)`**: the spec has `P` bind `?g` and the post-join then
///   filters on it; on the leaf-bound side `?g` is already bound, which
///   SPARQL does not even allow.
/// * **Any mention of `?g` in an `OPTIONAL`'s right arm**: the spec lets the
///   right arm bind `?g` from data (or leave it unbound and receive the graph
///   name afterwards), so the post-join drops left rows whose optional match
///   named a *different* graph. Leaf-binding pre-constrains the right arm to
///   this graph instead, changing both what matches and which left rows
///   survive (W3C `graph-optional`). This holds however the right arm is
///   built, so it is a *mention* test, not a barrier test.
///
/// A subtree that reads no quads is exempt: no scan leaf means no injected
/// column, so it already evaluates exactly as the spec says. The `OPTIONAL`
/// right-arm rule is deliberately outside that exemption — `OPTIONAL { VALUES
/// ?g { … } }` binds `?g` with no scan at all and still diverges.
///
/// Lifting any of this needs the graph variable joined *after* the block is
/// evaluated, rather than bound on the leaf — the per-graph block evaluation
/// SPEC-28 phase 3 deliberately did not build.
fn per_graph_var_divergence(plan: &LogicalPlan, var: &Var) -> Option<&'static str> {
    use LogicalPlan::*;
    // No scan leaf below ⇒ no injected graph column ⇒ nothing to diverge.
    if reads_no_quads(plan) {
        return None;
    }
    match plan {
        // The equivalent case: a data-supplied binding, inner-joined.
        Bgp { .. } | Values { .. } => None,
        Filter { expr, inner } => refs(expr, var)
            .then_some("a FILTER that references ?g")
            .or_else(|| per_graph_var_divergence(inner, var)),
        Extend {
            inner,
            var: target,
            expr,
        } => {
            if target == var {
                Some("a BIND to ?g")
            } else if refs(expr, var) {
                Some("a BIND that references ?g")
            } else {
                per_graph_var_divergence(inner, var)
            }
        }
        OrderBy { inner, keys } => keys
            .iter()
            .any(|(e, _)| refs(e, var))
            .then_some("an ORDER BY that references ?g")
            .or_else(|| per_graph_var_divergence(inner, var)),
        LeftJoin { left, right, expr } => {
            if expr.as_ref().is_some_and(|e| refs(e, var)) {
                Some("an OPTIONAL condition that references ?g")
            } else if mentions_var(right, var) {
                Some("an OPTIONAL that references ?g")
            } else {
                per_graph_var_divergence(left, var).or_else(|| per_graph_var_divergence(right, var))
            }
        }
        Join { left, right } | Union { left, right } => {
            per_graph_var_divergence(left, var).or_else(|| per_graph_var_divergence(right, var))
        }
        // `per_graph_barrier` refuses these inside a `GRAPH ?g` before this
        // check runs, so these arms are unreachable in practice — kept
        // exhaustive (no wildcard) so a new variant has to be classified.
        Group {
            inner,
            keys,
            aggregates,
        } => {
            let in_agg = aggregates.iter().any(|a| {
                a.out == *var
                    || crate::exec::runtime::agg_inner_exprs(a)
                        .into_iter()
                        .any(|e| refs(e, var))
            });
            (keys.contains(var) || in_agg)
                .then_some("a GROUP BY or aggregate that references ?g")
                .or_else(|| per_graph_var_divergence(inner, var))
        }
        Project { inner, .. } | Distinct { inner } | Slice { inner, .. } => {
            per_graph_var_divergence(inner, var)
        }
        PathClosure { edge, .. } => per_graph_var_divergence(edge, var),
    }
}

/// Does `expr` reference `var`?
fn refs(expr: &Expr, var: &Var) -> bool {
    let mut names = HashSet::new();
    crate::exec::runtime::referenced_vars(expr, &mut names);
    names.contains(var.name())
}

/// Does `var` appear **anywhere** in `plan` — pattern position, `VALUES`
/// column, `BIND` target, or any expression? Used for the `OPTIONAL`
/// right-arm rule, which cares that the arm touches `?g` at all, not how.
fn mentions_var(plan: &LogicalPlan, var: &Var) -> bool {
    use LogicalPlan::*;
    fn in_pattern(p: &TriplePattern, var: &Var) -> bool {
        [&p.subject, &p.predicate, &p.object]
            .into_iter()
            .any(|t| match t {
                Term::Var(v) => v == var,
                Term::Triple(inner) => in_pattern(inner, var),
                _ => false,
            })
    }
    match plan {
        Bgp { patterns, .. } => patterns.iter().any(|p| in_pattern(p, var)),
        Values { vars, .. } => vars.contains(var),
        Filter { expr, inner } => refs(expr, var) || mentions_var(inner, var),
        Extend {
            inner,
            var: target,
            expr,
        } => target == var || refs(expr, var) || mentions_var(inner, var),
        OrderBy { inner, keys } => {
            keys.iter().any(|(e, _)| refs(e, var)) || mentions_var(inner, var)
        }
        LeftJoin { left, right, expr } => {
            expr.as_ref().is_some_and(|e| refs(e, var))
                || mentions_var(left, var)
                || mentions_var(right, var)
        }
        Join { left, right } | Union { left, right } => {
            mentions_var(left, var) || mentions_var(right, var)
        }
        Project { inner, vars } => vars.contains(var) || mentions_var(inner, var),
        Distinct { inner } | Slice { inner, .. } => mentions_var(inner, var),
        Group {
            inner,
            keys,
            aggregates,
        } => {
            keys.contains(var)
                || aggregates.iter().any(|a| {
                    a.out == *var
                        || crate::exec::runtime::agg_inner_exprs(a)
                            .into_iter()
                            .any(|e| refs(e, var))
                })
                || mentions_var(inner, var)
        }
        PathClosure {
            subject,
            object,
            edge,
            ..
        } => {
            [subject, object]
                .into_iter()
                .any(|t| matches!(t, Term::Var(v) if v == var))
                || mentions_var(edge, var)
        }
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
