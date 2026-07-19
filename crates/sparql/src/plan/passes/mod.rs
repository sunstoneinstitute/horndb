//! Shared helpers for the heuristic logical-plan passes (SPEC-23 §5.2):
//! [`normalize`], [`filter_pullup`], [`filter_pushdown`], and the
//! projection-pushdown pass that lands in a later Phase-2 task.
//!
//! These operate on [`LogicalPlan`] — the same shapes `pass.rs`'s
//! `dangling_refs` and `pushdown.rs`'s physical-plan helpers already walk,
//! kept separate because the logical and physical IRs diverge (no
//! `BgpScan`/`CountScan`/`GroupCountScan` split at this layer yet).

pub mod filter_pullup;
pub mod filter_pushdown;
pub mod normalize;
pub use filter_pullup::FilterPullup;
pub use filter_pushdown::FilterPushdown;
pub use normalize::Normalize;

use crate::algebra::{Expr, Term, TriplePattern, Var};
use crate::plan::logical::LogicalPlan;
use crate::plan::types::infer;
use std::collections::HashSet;

/// A node's natural output variables, computed structurally, in
/// deterministic first-appearance order. Mirrors `pushdown::output_vars`
/// (the physical-plan equivalent) but returns `Var`s over the logical IR.
pub(crate) fn schema(node: &LogicalPlan) -> Vec<Var> {
    use LogicalPlan::*;
    match node {
        Bgp { patterns } => {
            let mut out = Vec::new();
            for p in patterns {
                push_pattern_vars(p, &mut out);
            }
            out
        }
        Join { left, right } | LeftJoin { left, right, .. } | Union { left, right } => {
            let mut out = schema(left);
            for v in schema(right) {
                push_unique(&mut out, v);
            }
            out
        }
        Filter { inner, .. } | Distinct { inner } | Slice { inner, .. } | OrderBy { inner, .. } => {
            schema(inner)
        }
        Project { vars, .. } => vars.clone(),
        Extend { inner, var, .. } => {
            let mut out = schema(inner);
            push_unique(&mut out, var.clone());
            out
        }
        Values { vars, .. } => vars.clone(),
        Group {
            keys, aggregates, ..
        } => {
            let mut out = keys.clone();
            for a in aggregates {
                push_unique(&mut out, a.out.clone());
            }
            out
        }
        PathClosure {
            subject, object, ..
        } => {
            let mut out = Vec::new();
            for t in [subject, object] {
                if let Term::Var(v) = t {
                    push_unique(&mut out, v.clone());
                }
            }
            out
        }
    }
}

fn push_unique(out: &mut Vec<Var>, v: Var) {
    if !out.contains(&v) {
        out.push(v);
    }
}

fn push_pattern_vars(p: &TriplePattern, out: &mut Vec<Var>) {
    for t in [&p.subject, &p.predicate, &p.object] {
        push_term_vars(t, out);
    }
}

fn push_term_vars(t: &Term, out: &mut Vec<Var>) {
    match t {
        Term::Var(v) => push_unique(out, v.clone()),
        Term::Triple(tp) => push_pattern_vars(tp, out),
        _ => {}
    }
}

/// Flatten a top-level `Expr::And` chain into `out`, consuming `expr`.
/// Non-`And` nodes (including nested `Or`/`Not` subtrees) are pushed as-is —
/// only the outermost conjunction is split.
pub(crate) fn conjuncts(expr: Expr, out: &mut Vec<Expr>) {
    match expr {
        Expr::And(a, b) => {
            conjuncts(*a, out);
            conjuncts(*b, out);
        }
        other => out.push(other),
    }
}

/// Rebuild a right-leaning `Expr::And` chain from `parts`. Empty input
/// folds to `None` (the caller drops the `Filter` entirely).
pub(crate) fn conjoin(mut parts: Vec<Expr>) -> Option<Expr> {
    let last = parts.pop()?;
    Some(
        parts
            .into_iter()
            .rev()
            .fold(last, |acc, e| Expr::And(Box::new(e), Box::new(acc))),
    )
}

/// The names of every variable `node` provably binds (`TypeMask::is_bound`)
/// — the legality test [`filter_pullup`] (and later filter-pushdown) reads
/// to decide whether a predicate can move past a subtree without changing
/// which rows survive.
pub(crate) fn bound_vars(node: &LogicalPlan) -> HashSet<String> {
    let types = infer(node);
    types
        .vars()
        .filter(|v| types.get(v).is_some_and(|m| m.is_bound()))
        .map(|v| v.name().to_owned())
        .collect()
}

/// Apply `f` to each direct child of `node`, rebuilding `node` with the
/// rewritten children. Leaves (`Bgp`, `Values`) are returned unchanged.
/// Mirrors `pushdown::map_children` (the physical-plan equivalent), but
/// takes `&dyn Fn` rather than `fn` so a pass can close over state.
pub(crate) fn map_children(
    node: LogicalPlan,
    f: &dyn Fn(LogicalPlan) -> LogicalPlan,
) -> LogicalPlan {
    use LogicalPlan::*;
    match node {
        leaf @ (Bgp { .. } | Values { .. }) => leaf,
        Join { left, right } => Join {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
        },
        LeftJoin { left, right, expr } => LeftJoin {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
            expr,
        },
        Filter { expr, inner } => Filter {
            expr,
            inner: Box::new(f(*inner)),
        },
        Union { left, right } => Union {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
        },
        Project { vars, inner } => Project {
            vars,
            inner: Box::new(f(*inner)),
        },
        Distinct { inner } => Distinct {
            inner: Box::new(f(*inner)),
        },
        Slice {
            inner,
            start,
            length,
        } => Slice {
            inner: Box::new(f(*inner)),
            start,
            length,
        },
        OrderBy { inner, keys } => OrderBy {
            inner: Box::new(f(*inner)),
            keys,
        },
        Extend { inner, var, expr } => Extend {
            inner: Box::new(f(*inner)),
            var,
            expr,
        },
        Group {
            inner,
            keys,
            aggregates,
        } => Group {
            inner: Box::new(f(*inner)),
            keys,
            aggregates,
        },
        PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PathClosure {
            subject,
            object,
            edge: Box::new(f(*edge)),
            reflexive,
        },
    }
}
