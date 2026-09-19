//! `EXISTS { P }` as an expression (SPARQL 1.1 §18.6).
//!
//! §18.6 evaluates `EXISTS` once per solution, not once per query: take the
//! current solution mapping μ, **substitute** it into `P` (every variable μ
//! binds becomes that term; every other variable stays free), evaluate the
//! substituted pattern, and answer "did it produce at least one row?".
//! `NOT EXISTS` is the negation of that same per-solution answer — the
//! translator hands it over as `Not(Exists(..))`, so there is one code path.
//!
//! Two things fall out of substitution that a join against μ would get wrong,
//! which is why this is not implemented as a join:
//!
//! * A variable bound in μ is replaced *everywhere* in `P`, including inside
//!   a sub-`SELECT` that projects it away or a `MINUS` right-hand side. A
//!   join only ever constrains the variables `P` still exports.
//! * A variable that only occurs inside `P` is not in `dom(μ)`, so it stays
//!   free and ranges over the data — it is never correlated with an outer
//!   variable that happens to share its name but is unbound in this row.
//!
//! ponytail: the substituted pattern is planned and run from scratch for
//! every row. That is O(rows) planning passes. Cache the plan keyed by the
//! substituted terms if a real workload ever makes this hot.

use crate::algebra::{Aggregate, Algebra, Expr, GraphSpec, Term, TriplePattern};
use crate::error::Result;
use crate::exec::{Bindings, Executor};
use crate::plan::planner;
use crate::DefaultGraphMode;
use std::collections::HashSet;

/// Evaluate `EXISTS { pattern }` under the solution mapping `mu`.
pub(crate) fn eval_exists<E: Executor + ?Sized>(
    exec: &E,
    dataset: &crate::algebra::DatasetSpec,
    mode: DefaultGraphMode,
    base: Option<&str>,
    pattern: &Algebra,
    mu: &Bindings,
) -> Result<bool> {
    let substituted = substitute(pattern, mu);
    let plan = planner::plan(&substituted)?;
    let rt = super::runtime::Runtime::new(exec)
        .with_dataset(dataset.clone(), mode)
        .with_base(base.map(str::to_owned));
    let mut stream = rt.run_stream(&plan)?;
    while let Some(chunk) = stream.next_chunk()? {
        if !chunk.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every variable name mentioned anywhere in `alg`.
///
/// `referenced_vars` reports these for an `Expr::Exists`, which is what makes
/// the correlated columns survive column pruning and stops `FilterPushdown`
/// from sinking the filter below the node that binds them. Reporting the free
/// (inner-only) variables too is deliberate: it is conservative in the safe
/// direction — a filter whose reported variables no arm binds simply stays
/// where it is.
pub(crate) fn pattern_vars(alg: &Algebra, out: &mut HashSet<String>) {
    fn term(t: &Term, out: &mut HashSet<String>) {
        match t {
            Term::Var(v) => {
                out.insert(v.name().to_owned());
            }
            Term::Triple(tp) => {
                term(&tp.subject, out);
                term(&tp.predicate, out);
                term(&tp.object, out);
            }
            _ => {}
        }
    }
    fn expr(e: &Expr, out: &mut HashSet<String>) {
        super::runtime::referenced_vars(e, out);
    }

    match alg {
        Algebra::Bgp { patterns } => {
            for p in patterns {
                term(&p.subject, out);
                term(&p.predicate, out);
                term(&p.object, out);
            }
        }
        Algebra::Join { left, right } | Algebra::Union { left, right } => {
            pattern_vars(left, out);
            pattern_vars(right, out);
        }
        // MINUS's right arm never exports columns, but its variables still
        // decide which left rows survive, so they are referenced.
        Algebra::Minus { left, right } => {
            pattern_vars(left, out);
            pattern_vars(right, out);
        }
        Algebra::LeftJoin {
            left,
            right,
            expr: e,
        } => {
            pattern_vars(left, out);
            pattern_vars(right, out);
            if let Some(e) = e {
                expr(e, out);
            }
        }
        Algebra::Filter { expr: e, inner } => {
            expr(e, out);
            pattern_vars(inner, out);
        }
        Algebra::Project { vars, inner } => {
            for v in vars {
                out.insert(v.name().to_owned());
            }
            pattern_vars(inner, out);
        }
        Algebra::Distinct { inner } | Algebra::Slice { inner, .. } => pattern_vars(inner, out),
        Algebra::OrderBy { inner, keys } => {
            pattern_vars(inner, out);
            for (e, _) in keys {
                expr(e, out);
            }
        }
        Algebra::Extend {
            inner,
            var,
            expr: e,
        } => {
            pattern_vars(inner, out);
            out.insert(var.name().to_owned());
            expr(e, out);
        }
        Algebra::Values { vars, .. } => {
            for v in vars {
                out.insert(v.name().to_owned());
            }
        }
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => {
            pattern_vars(inner, out);
            for k in keys {
                out.insert(k.name().to_owned());
            }
            for a in aggregates {
                out.insert(a.out.name().to_owned());
                for e in super::runtime::agg_inner_exprs(a) {
                    expr(e, out);
                }
            }
        }
        Algebra::PathClosure {
            subject,
            object,
            edge,
            ..
        } => {
            term(subject, out);
            term(object, out);
            pattern_vars(edge, out);
        }
        Algebra::Graph { name, inner } => {
            if let GraphSpec::Var(v) = name {
                out.insert(v.name().to_owned());
            }
            pattern_vars(inner, out);
        }
    }
}

/// §18.6 `substitute(P, μ)`: replace every occurrence of a variable bound in
/// `mu` with the term it is bound to. Variables `mu` does not bind are left
/// alone — they stay free and range over the data.
fn substitute(alg: &Algebra, mu: &Bindings) -> Algebra {
    match alg {
        Algebra::Bgp { patterns } => Algebra::Bgp {
            patterns: patterns.iter().map(|p| sub_triple(p, mu)).collect(),
        },
        Algebra::Join { left, right } => Algebra::Join {
            left: Box::new(substitute(left, mu)),
            right: Box::new(substitute(right, mu)),
        },
        Algebra::Union { left, right } => Algebra::Union {
            left: Box::new(substitute(left, mu)),
            right: Box::new(substitute(right, mu)),
        },
        Algebra::Minus { left, right } => Algebra::Minus {
            left: Box::new(substitute(left, mu)),
            right: Box::new(substitute(right, mu)),
        },
        Algebra::LeftJoin { left, right, expr } => Algebra::LeftJoin {
            left: Box::new(substitute(left, mu)),
            right: Box::new(substitute(right, mu)),
            expr: expr.as_ref().map(|e| sub_expr(e, mu)),
        },
        Algebra::Filter { expr, inner } => Algebra::Filter {
            expr: sub_expr(expr, mu),
            inner: Box::new(substitute(inner, mu)),
        },
        // A projected variable that substitution turned into a term is no
        // longer a variable, so it drops out of the projection list.
        Algebra::Project { vars, inner } => Algebra::Project {
            vars: vars
                .iter()
                .filter(|v| mu.get(v.name()).is_none())
                .cloned()
                .collect(),
            inner: Box::new(substitute(inner, mu)),
        },
        Algebra::Distinct { inner } => Algebra::Distinct {
            inner: Box::new(substitute(inner, mu)),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => Algebra::Slice {
            inner: Box::new(substitute(inner, mu)),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => Algebra::OrderBy {
            inner: Box::new(substitute(inner, mu)),
            keys: keys.iter().map(|(e, d)| (sub_expr(e, mu), *d)).collect(),
        },
        Algebra::Extend { inner, var, expr } => Algebra::Extend {
            inner: Box::new(substitute(inner, mu)),
            var: var.clone(),
            expr: sub_expr(expr, mu),
        },
        // Substituting a VALUES column means keeping only the rows that agree
        // with μ on it, then dropping the column — the same "replace the
        // variable by the term" rule expressed over a literal row set.
        Algebra::Values { vars, rows } => {
            let keep: Vec<usize> = (0..vars.len())
                .filter(|&i| mu.get(vars[i].name()).is_none())
                .collect();
            let rows = rows
                .iter()
                .filter(|r| {
                    vars.iter()
                        .enumerate()
                        .all(|(i, v)| match mu.get(v.name()) {
                            Some(t) => r.get(i).and_then(Option::as_ref) == Some(t),
                            None => true,
                        })
                })
                .map(|r| keep.iter().map(|&i| r.get(i).cloned().flatten()).collect())
                .collect();
            Algebra::Values {
                vars: keep.iter().map(|&i| vars[i].clone()).collect(),
                rows,
            }
        }
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => Algebra::Group {
            inner: Box::new(substitute(inner, mu)),
            keys: keys.clone(),
            aggregates: aggregates.iter().map(|a| sub_agg(a, mu)).collect(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => Algebra::PathClosure {
            subject: sub_term(subject, mu),
            object: sub_term(object, mu),
            edge: Box::new(substitute(edge, mu)),
            reflexive: *reflexive,
        },
        // `GRAPH ?g` with `?g` bound in μ narrows to that one graph.
        Algebra::Graph { name, inner } => Algebra::Graph {
            name: match name {
                GraphSpec::Var(v) => match mu.get(v.name()) {
                    Some(Term::Iri(iri)) => GraphSpec::Iri(iri.clone()),
                    _ => GraphSpec::Var(v.clone()),
                },
                iri => iri.clone(),
            },
            inner: Box::new(substitute(inner, mu)),
        },
    }
}

fn sub_term(t: &Term, mu: &Bindings) -> Term {
    match t {
        Term::Var(v) => mu.get(v.name()).cloned().unwrap_or_else(|| t.clone()),
        Term::Triple(tp) => Term::Triple(Box::new(sub_triple(tp, mu))),
        other => other.clone(),
    }
}

fn sub_triple(p: &TriplePattern, mu: &Bindings) -> TriplePattern {
    TriplePattern {
        subject: sub_term(&p.subject, mu),
        predicate: sub_term(&p.predicate, mu),
        object: sub_term(&p.object, mu),
    }
}

fn sub_agg(a: &Aggregate, mu: &Bindings) -> Aggregate {
    use crate::algebra::AggFunc as A;
    let f = match &a.func {
        A::CountStar => A::CountStar,
        A::Count(e) => A::Count(Box::new(sub_expr(e, mu))),
        A::Sum(e) => A::Sum(Box::new(sub_expr(e, mu))),
        A::Min(e) => A::Min(Box::new(sub_expr(e, mu))),
        A::Max(e) => A::Max(Box::new(sub_expr(e, mu))),
        A::Avg(e) => A::Avg(Box::new(sub_expr(e, mu))),
        A::Sample(e) => A::Sample(Box::new(sub_expr(e, mu))),
        A::GroupConcat { expr, separator } => A::GroupConcat {
            expr: Box::new(sub_expr(expr, mu)),
            separator: separator.clone(),
        },
    };
    Aggregate {
        out: a.out.clone(),
        func: f,
        distinct: a.distinct,
    }
}

fn sub_expr(e: &Expr, mu: &Bindings) -> Expr {
    let b = |x: &Expr| Box::new(sub_expr(x, mu));
    let list = |xs: &Vec<Expr>| xs.iter().map(|x| sub_expr(x, mu)).collect::<Vec<_>>();
    match e {
        Expr::Term(t) => Expr::Term(sub_term(t, mu)),
        // `BOUND(?v)` with `?v` substituted away asks about a term, which is
        // always bound.
        Expr::Bound(v) => match mu.get(v.name()) {
            Some(_) => Expr::Term(Term::Literal(TRUE.to_owned())),
            None => Expr::Bound(v.clone()),
        },
        Expr::Eq(x, y) => Expr::Eq(b(x), b(y)),
        Expr::SameTerm(x, y) => Expr::SameTerm(b(x), b(y)),
        Expr::Ne(x, y) => Expr::Ne(b(x), b(y)),
        Expr::Lt(x, y) => Expr::Lt(b(x), b(y)),
        Expr::Gt(x, y) => Expr::Gt(b(x), b(y)),
        Expr::Le(x, y) => Expr::Le(b(x), b(y)),
        Expr::Ge(x, y) => Expr::Ge(b(x), b(y)),
        Expr::And(x, y) => Expr::And(b(x), b(y)),
        Expr::Or(x, y) => Expr::Or(b(x), b(y)),
        Expr::Add(x, y) => Expr::Add(b(x), b(y)),
        Expr::Sub(x, y) => Expr::Sub(b(x), b(y)),
        Expr::Mul(x, y) => Expr::Mul(b(x), b(y)),
        Expr::Div(x, y) => Expr::Div(b(x), b(y)),
        Expr::Not(x) => Expr::Not(b(x)),
        Expr::Neg(x) => Expr::Neg(b(x)),
        Expr::If(c, t, f) => Expr::If(b(c), b(t), b(f)),
        Expr::In(x, xs) => Expr::In(b(x), list(xs)),
        Expr::Coalesce(xs) => Expr::Coalesce(list(xs)),
        Expr::Func(f, xs) => Expr::Func(*f, list(xs)),
        // A nested EXISTS is part of P, so μ substitutes into it too; its own
        // per-row substitution then extends this one.
        Expr::Exists(p) => Expr::Exists(Box::new(substitute(p, mu))),
    }
}

const TRUE: &str = "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;

    fn var(n: &str) -> Term {
        Term::Var(Var::new(n))
    }

    fn mu(pairs: &[(&str, Term)]) -> Bindings {
        let mut b = Bindings::new();
        for (k, v) in pairs {
            b.set((*k).to_owned(), v.clone());
        }
        b
    }

    /// A variable bound in μ becomes its term; one μ does not bind stays free.
    #[test]
    fn substitution_replaces_only_bound_vars() {
        let bgp = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("p".into()),
                object: var("o"),
            }],
        };
        let out = substitute(&bgp, &mu(&[("s", Term::Iri("urn:a".into()))]));
        let Algebra::Bgp { patterns } = out else {
            panic!("expected Bgp");
        };
        assert_eq!(patterns[0].subject, Term::Iri("urn:a".into()));
        assert_eq!(patterns[0].object, var("o"), "?o is not in dom(mu)");
    }

    /// §18.6 substitutes into the whole pattern, including a sub-SELECT that
    /// projects the variable away — the case a join against μ gets wrong.
    #[test]
    fn substitution_reaches_under_a_projection() {
        let inner = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: var("x"),
                predicate: Term::Iri("p".into()),
                object: var("y"),
            }],
        };
        let alg = Algebra::Project {
            vars: vec![Var::new("y"), Var::new("x")],
            inner: Box::new(inner),
        };
        let out = substitute(&alg, &mu(&[("x", Term::Iri("urn:a".into()))]));
        let Algebra::Project { vars, inner } = out else {
            panic!("expected Project");
        };
        assert_eq!(vars, vec![Var::new("y")], "?x is no longer a variable");
        let Algebra::Bgp { patterns } = *inner else {
            panic!("expected Bgp");
        };
        assert_eq!(patterns[0].subject, Term::Iri("urn:a".into()));
    }

    /// `pattern_vars` reports both the correlated and the inner-only
    /// variables, so column pruning keeps the columns EXISTS reads.
    #[test]
    fn pattern_vars_reports_free_and_correlated() {
        let alg = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: var("outer"),
                predicate: Term::Iri("p".into()),
                object: var("innerOnly"),
            }],
        };
        let mut vars = HashSet::new();
        pattern_vars(&alg, &mut vars);
        assert!(vars.contains("outer"));
        assert!(vars.contains("innerOnly"));
    }
}
