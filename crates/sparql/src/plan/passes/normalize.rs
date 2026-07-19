//! `Normalize` (SPEC-23 §5.2): boolean-connective simplification + constant
//! filter folding + `Eq -> SameTerm` strength reduction. Single-shot,
//! statistics-free, result-invariant.

use crate::algebra::{Expr, Term};
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{conjoin, conjuncts, map_children, schema};
use crate::plan::types::{infer, VarTypes};

pub struct Normalize;

impl LogicalPass for Normalize {
    fn id(&self) -> PassId {
        PassId::Normalize
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        normalize(plan)
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::CoalesceBgp]
    }
}

/// Bottom-up: normalize every child first, then simplify a `Filter` at this
/// node using its (already-normalized) inner's type lattice.
fn normalize(plan: LogicalPlan) -> LogicalPlan {
    let plan = map_children(plan, &normalize);
    match plan {
        LogicalPlan::Filter { expr, inner } => normalize_filter(expr, *inner),
        other => other,
    }
}

/// Split `expr` into conjuncts, strength-reduce and constant-fold each one
/// against `inner`'s type lattice, then rebuild:
/// * a dropped (constant-true) conjunct just vanishes;
/// * a constant-false conjunct makes the whole filter unsatisfiable — the
///   result is an empty relation carrying `inner`'s schema, so anything
///   above still sees the right output variables;
/// * with nothing left, the `Filter` itself is redundant — return `inner`.
fn normalize_filter(expr: Expr, inner: LogicalPlan) -> LogicalPlan {
    let types = infer(&inner);
    let mut parts = Vec::new();
    conjuncts(expr, &mut parts);

    let mut kept = Vec::new();
    for part in parts {
        let reduced = reduce_expr(part, &types);
        match const_bool(&reduced) {
            Some(true) => {}
            Some(false) => {
                return LogicalPlan::Values {
                    vars: schema(&inner),
                    rows: vec![],
                };
            }
            None => kept.push(reduced),
        }
    }

    match conjoin(kept) {
        Some(expr) => LogicalPlan::Filter {
            expr,
            inner: Box::new(inner),
        },
        None => inner,
    }
}

/// Recurse through boolean connectives, reducing `Eq(a, b)` to `SameTerm(a,
/// b)` wherever the lattice proves both sides the same non-literal kind
/// (`same_nonliteral_kind`). Every other expression shape is left as-is —
/// Normalize does not fold arithmetic or non-boolean builtins.
fn reduce_expr(expr: Expr, types: &VarTypes) -> Expr {
    match expr {
        Expr::And(a, b) => Expr::And(
            Box::new(reduce_expr(*a, types)),
            Box::new(reduce_expr(*b, types)),
        ),
        Expr::Or(a, b) => Expr::Or(
            Box::new(reduce_expr(*a, types)),
            Box::new(reduce_expr(*b, types)),
        ),
        Expr::Not(a) => Expr::Not(Box::new(reduce_expr(*a, types))),
        Expr::Eq(a, b) if same_nonliteral_kind(&a, &b, types) => Expr::SameTerm(a, b),
        other => other,
    }
}

/// True iff both operands are `Expr::Term` and provably the same non-literal
/// kind: both IRIs (a constant `Term::Iri`, or a variable whose inferred
/// mask is exactly `NamedNode`), or both blank nodes. Literals are excluded
/// — `Eq` over a literal-shaped operand may later gain value-equality
/// semantics (numeric promotion) that `SameTerm`'s structural comparison
/// does not have, so reducing there would not be result-invariant. This
/// also keeps `?v = "lit"` un-reduced, preserving `pushdown::eq_conjuncts`'s
/// literal-constant inlining.
fn same_nonliteral_kind(a: &Expr, b: &Expr, types: &VarTypes) -> bool {
    let (Expr::Term(a), Expr::Term(b)) = (a, b) else {
        return false;
    };
    (provably_iri(a, types) && provably_iri(b, types))
        || (provably_blank(a, types) && provably_blank(b, types))
}

fn provably_iri(t: &Term, types: &VarTypes) -> bool {
    match t {
        Term::Iri(_) => true,
        Term::Var(v) => types.get(v).is_some_and(|m| m.is_named_node()),
        _ => false,
    }
}

fn provably_blank(t: &Term, types: &VarTypes) -> bool {
    match t {
        Term::BlankNode(_) => true,
        Term::Var(v) => types.get(v).is_some_and(|m| m.is_blank_node()),
        _ => false,
    }
}

/// Evaluate `e` at plan-build time when every leaf is a ground constant.
/// `And`/`Or` short-circuit on a decisive branch even when the other side is
/// not (itself) constant. Everything else — including any expression that
/// still contains a variable — is `None` (unknown at this stage).
fn const_bool(e: &Expr) -> Option<bool> {
    match e {
        Expr::Eq(a, b) | Expr::SameTerm(a, b) => const_term_eq(a, b),
        Expr::Ne(a, b) => const_term_eq(a, b).map(|eq| !eq),
        Expr::And(a, b) => match (const_bool(a), const_bool(b)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Expr::Or(a, b) => match (const_bool(a), const_bool(b)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        Expr::Not(a) => const_bool(a).map(|v| !v),
        _ => None,
    }
}

/// `Some(a == b)` when both sides are ground constant terms
/// (`Iri`/`BlankNode`/`Literal` — never `Var`, never an RDF 1.2 `Triple`
/// term, which this pass does not attempt to compare structurally).
fn const_term_eq(a: &Expr, b: &Expr) -> Option<bool> {
    match (a, b) {
        (Expr::Term(a), Expr::Term(b)) if is_const_term(a) && is_const_term(b) => Some(a == b),
        _ => None,
    }
}

fn is_const_term(t: &Term) -> bool {
    matches!(t, Term::Iri(_) | Term::BlankNode(_) | Term::Literal(_))
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
        PlanCtx {
            disabled_passes: Default::default(),
        }
    }

    /// A predicate-position variable is provably a NamedNode, so
    /// `Eq(?p, <iri>)` reduces to `SameTerm`.
    #[test]
    fn reduces_eq_on_provable_iri_to_sameterm() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let eq = Expr::Eq(
            Box::new(Expr::Term(var("p"))),
            Box::new(Expr::Term(Term::Iri("http://ex/knows".into()))),
        );
        let plan = LogicalPlan::Filter {
            expr: eq,
            inner: Box::new(bgp),
        };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Filter { expr, .. } = out else {
            panic!("expected Filter, got {out:?}")
        };
        assert!(
            matches!(expr, Expr::SameTerm(..)),
            "predicate-var Eq must reduce; got {expr:?}"
        );
    }

    /// An object-position variable's kind is NOT provably singular (could be
    /// literal), so an `Eq` against a literal constant must NOT reduce —
    /// this preserves the physical count-pushdown equality inlining.
    #[test]
    fn keeps_eq_on_unprovable_kind() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("http://ex/name".into()),
                object: var("o"),
            }],
        };
        let eq = Expr::Eq(
            Box::new(Expr::Term(var("o"))),
            Box::new(Expr::Term(Term::Literal("\"Alice\"".into()))),
        );
        let plan = LogicalPlan::Filter {
            expr: eq,
            inner: Box::new(bgp),
        };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Filter { expr, .. } = out else {
            panic!("expected Filter")
        };
        assert!(
            matches!(expr, Expr::Eq(..)),
            "literal-side Eq must NOT reduce; got {expr:?}"
        );
    }

    /// A constant-true conjunct is dropped; if it was the whole predicate,
    /// the Filter is removed.
    #[test]
    fn drops_constant_true_filter() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let t = Expr::Eq(
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
        );
        let plan = LogicalPlan::Filter {
            expr: t,
            inner: Box::new(bgp),
        };
        let out = Normalize.run(plan, &ctx());
        assert!(
            matches!(out, LogicalPlan::Bgp { .. }),
            "true filter must be dropped; got {out:?}"
        );
    }

    /// A constant-false filter becomes an empty relation carrying the inner
    /// schema.
    #[test]
    fn empties_constant_false_filter() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let f = Expr::Eq(
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
            Box::new(Expr::Term(Term::Iri("http://ex/b".into()))),
        );
        let plan = LogicalPlan::Filter {
            expr: f,
            inner: Box::new(bgp),
        };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Values { vars, rows } = out else {
            panic!("expected empty Values, got {out:?}")
        };
        assert!(rows.is_empty());
        assert_eq!(vars, vec![Var::new("s"), Var::new("p"), Var::new("o")]);
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(Normalize.id(), PassId::Normalize);
        assert_eq!(Normalize.must_follow(), &[PassId::CoalesceBgp]);
    }
}
