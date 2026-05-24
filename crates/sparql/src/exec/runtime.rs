//! Iterator-style runtime over [`PhysicalPlan`]. Each plan node
//! yields a `Vec<Bindings>` because Stage 1 materialises per-node;
//! true streaming is a Future Work item.

use crate::algebra::{Expr, OrderDir, Term, Var};
use crate::error::{Result, SparqlError};
use crate::exec::{Bindings, Executor};
use crate::plan::PhysicalPlan;

pub struct Runtime<'a, E: Executor + ?Sized> {
    exec: &'a E,
}

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    pub fn new(exec: &'a E) -> Self {
        Self { exec }
    }

    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let v = self.eval(plan)?;
        Ok(v.into_iter())
    }

    fn eval(&self, plan: &PhysicalPlan) -> Result<Vec<Bindings>> {
        match plan {
            PhysicalPlan::BgpScan { patterns } => Ok(self.exec.scan_bgp(patterns)?.collect()),
            PhysicalPlan::Join { left, right } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                let mut out = Vec::new();
                for a in &l {
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            out.push(m);
                        }
                    }
                }
                Ok(out)
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                let mut out = Vec::new();
                for a in &l {
                    let mut matched = false;
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            let keep = match expr {
                                Some(e) => eval_expr(e, &m)?,
                                None => true,
                            };
                            if keep {
                                matched = true;
                                out.push(m);
                            }
                        }
                    }
                    if !matched {
                        out.push(a.clone());
                    }
                }
                Ok(out)
            }
            PhysicalPlan::Filter { expr, inner } => {
                let v = self.eval(inner)?;
                v.into_iter()
                    .map(|b| eval_expr(expr, &b).map(|keep| (b, keep)))
                    .collect::<Result<Vec<_>>>()
                    .map(|pairs| {
                        pairs
                            .into_iter()
                            .filter(|(_, k)| *k)
                            .map(|(b, _)| b)
                            .collect()
                    })
            }
            PhysicalPlan::Union { left, right } => {
                let mut a = self.eval(left)?;
                let b = self.eval(right)?;
                a.extend(b);
                Ok(a)
            }
            PhysicalPlan::Project { vars, inner } => {
                let v = self.eval(inner)?;
                Ok(v.into_iter().map(|b| project(&b, vars)).collect())
            }
            PhysicalPlan::Distinct { inner } => {
                let v = self.eval(inner)?;
                let mut seen: Vec<Bindings> = Vec::new();
                for b in v {
                    if !seen.contains(&b) {
                        seen.push(b);
                    }
                }
                Ok(seen)
            }
            PhysicalPlan::Slice {
                inner,
                start,
                length,
            } => {
                let v = self.eval(inner)?;
                let s = *start;
                let take = length.unwrap_or(v.len().saturating_sub(s));
                Ok(v.into_iter().skip(s).take(take).collect())
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let mut v = self.eval(inner)?;
                v.sort_by(|a, b| compare_by_keys(a, b, keys));
                Ok(v)
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                let v = self.eval(inner)?;
                let mut out = Vec::with_capacity(v.len());
                for mut b in v {
                    if let Some(t) = eval_expr_to_term(expr, &b)? {
                        b.set(var.name().to_owned(), t);
                    }
                    out.push(b);
                }
                Ok(out)
            }
            PhysicalPlan::Values { vars, rows } => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut b = Bindings::new();
                    for (var, cell) in vars.iter().zip(row.iter()) {
                        if let Some(term) = cell {
                            b.set(var.name().to_owned(), term.clone());
                        }
                    }
                    out.push(b);
                }
                Ok(out)
            }
        }
    }
}

fn project(b: &Bindings, vars: &[Var]) -> Bindings {
    if vars.is_empty() {
        // SELECT * with no projected vars (e.g. ASK): preserve.
        return b.clone();
    }
    let mut out = Bindings::new();
    for v in vars {
        if let Some(t) = b.get(v.name()) {
            out.set(v.name().to_owned(), t.clone());
        }
    }
    out
}

fn compare_by_keys(a: &Bindings, b: &Bindings, keys: &[(Expr, OrderDir)]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (e, dir) in keys {
        let ta = eval_expr_to_term(e, a).ok().flatten();
        let tb = eval_expr_to_term(e, b).ok().flatten();
        let ord = match (ta, tb) {
            (Some(x), Some(y)) => lex(&x).cmp(&lex(&y)),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return match dir {
                OrderDir::Asc => ord,
                OrderDir::Desc => ord.reverse(),
            };
        }
    }
    std::cmp::Ordering::Equal
}

fn lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
    }
}

fn eval_expr(e: &Expr, b: &Bindings) -> Result<bool> {
    Ok(match e {
        Expr::Eq(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::Ne(a, c) => eval_expr_to_term(a, b)? != eval_expr_to_term(c, b)?,
        Expr::Lt(a, c) => match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => lex(&x) < lex(&y),
            _ => false,
        },
        Expr::Gt(a, c) => match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => lex(&x) > lex(&y),
            _ => false,
        },
        Expr::And(a, c) => eval_expr(a, b)? && eval_expr(c, b)?,
        Expr::Or(a, c) => eval_expr(a, b)? || eval_expr(c, b)?,
        Expr::Not(a) => !eval_expr(a, b)?,
        Expr::Bound(v) => b.get(v.name()).is_some(),
        Expr::Term(t) => match t {
            // Bare term in boolean context: treat IRI/Literal as
            // truthy; var resolves to its binding.
            Term::Var(v) => b.get(v.name()).is_some(),
            _ => true,
        },
    })
}

fn eval_expr_to_term(e: &Expr, b: &Bindings) -> Result<Option<Term>> {
    Ok(match e {
        Expr::Term(t) => match t {
            Term::Var(v) => b.get(v.name()).cloned(),
            other => Some(other.clone()),
        },
        // Boolean-typed expressions return a typed literal (lexical
        // form "true"/"false"); good enough for Stage 1 BIND tests.
        Expr::Eq(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
        | Expr::Gt(_, _)
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::Bound(_) => Some(Term::Literal(
            if eval_expr(e, b)? { "true" } else { "false" }.into(),
        )),
    })
}

// Type-witness so we don't drop SparqlError from this module.
#[allow(dead_code)]
fn _witness() -> Result<()> {
    Err(SparqlError::Executor("unreachable".into()))
}

/// Render a CONSTRUCT template against a stream of solution mappings.
///
/// Returns concrete `(s, p, o)` lexical-form triples. Triples whose
/// template references an unbound variable in the row are skipped
/// (W3C: "ground triple results only").
pub fn construct_triples(
    query: &spargebra::Query,
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use spargebra::term::{NamedNodePattern, TermPattern};
    let template = match query {
        spargebra::Query::Construct { template, .. } => template,
        _ => {
            return Err(SparqlError::Executor(
                "construct_triples called on non-CONSTRUCT query".into(),
            ))
        }
    };

    fn resolve_term(t: &TermPattern, row: &Bindings) -> Option<String> {
        match t {
            TermPattern::NamedNode(n) => Some(n.as_str().to_owned()),
            TermPattern::BlankNode(b) => Some(b.as_str().to_owned()),
            TermPattern::Literal(l) => Some(l.to_string()),
            TermPattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => {
                    Some(s.clone())
                }
                _ => None,
            },
        }
    }
    fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<String> {
        match p {
            NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
            NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) => Some(s.clone()),
                _ => None,
            },
        }
    }

    let mut out = Vec::new();
    for row in rows {
        for tp in template {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&tp.subject, row),
                resolve_pred(&tp.predicate, row),
                resolve_term(&tp.object, row),
            ) {
                out.push((s, p, o));
            }
        }
    }
    Ok(out)
}
