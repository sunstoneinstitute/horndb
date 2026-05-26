//! Algebra translation from `spargebra` AST to our internal [`Algebra`].
//!
//! Stage 1 supports a deliberately small operator set; constructs we
//! do not yet handle return `SparqlError::UnsupportedAlgebra` (or
//! `UnsupportedPathOp` for the Kleene-star property paths) so the
//! planner never has to defend against them.

use crate::algebra::{Algebra, Expr, OrderDir, Term, TriplePattern, Var};
use crate::error::{Result, SparqlError};
use crate::SparqlConfig;
use spargebra::algebra::{Expression, GraphPattern, OrderExpression, PropertyPathExpression};
use spargebra::term::{
    GroundTerm, NamedNodePattern, TermPattern, TriplePattern as SpgTriplePattern, Variable,
};
use spargebra::Query;

/// Top-level entry: lower a parsed `spargebra::Query` to [`Algebra`]
/// using the default [`SparqlConfig`] (SPARQL 1.1 semantics — triple-term
/// patterns are rejected). For RDF 1.2 callers use
/// [`translate_query_with`].
pub fn translate_query(q: &Query) -> Result<Algebra> {
    translate_query_with(q, &SparqlConfig::default())
}

/// Like [`translate_query`] but takes an explicit [`SparqlConfig`] —
/// pass [`SparqlConfig::rdf12`] to accept triple-term patterns.
pub fn translate_query_with(q: &Query, cfg: &SparqlConfig) -> Result<Algebra> {
    match q {
        Query::Select {
            pattern,
            dataset: _,
            base_iri: _,
        } => {
            // spargebra often wraps the WHERE clause in a Project node
            // already (for the SELECT clause's variable list). If so,
            // honour it; otherwise wrap ourselves with the visible-var
            // list.
            if let GraphPattern::Project { inner, variables } = pattern {
                let inner_alg = translate_pattern(inner, cfg)?;
                Ok(Algebra::Project {
                    vars: variables.iter().map(translate_var).collect(),
                    inner: Box::new(inner_alg),
                })
            } else {
                let inner = translate_pattern(pattern, cfg)?;
                let vars = collect_visible_vars(pattern);
                Ok(Algebra::Project {
                    vars,
                    inner: Box::new(inner),
                })
            }
        }
        Query::Ask {
            pattern,
            dataset: _,
            base_iri: _,
        } => {
            let inner = translate_pattern(pattern, cfg)?;
            Ok(Algebra::Project {
                vars: Vec::new(),
                inner: Box::new(inner),
            })
        }
        Query::Construct {
            template: _,
            pattern,
            dataset: _,
            base_iri: _,
        } => {
            // The CONSTRUCT template is preserved separately by the
            // runtime; here we only return the WHERE-clause algebra.
            // The planner is responsible for re-attaching the
            // template via Runtime::run_construct.
            translate_pattern(pattern, cfg)
        }
        Query::Describe { .. } => Err(SparqlError::UnsupportedAlgebra("DESCRIBE".into())),
    }
}

/// Lower a `GraphPattern` (spargebra) to our `Algebra`.
fn translate_pattern(p: &GraphPattern, cfg: &SparqlConfig) -> Result<Algebra> {
    match p {
        GraphPattern::Bgp { patterns } => {
            let mut out = Vec::with_capacity(patterns.len());
            for tp in patterns {
                out.push(translate_triple(tp, cfg)?);
            }
            Ok(Algebra::Bgp { patterns: out })
        }
        GraphPattern::Path {
            subject,
            path,
            object,
        } => {
            // Stage 1 supports only Seq (`/`) and Inverse (`^`); both
            // expand to additional triple patterns (fresh variables
            // for the intermediate node in `Seq`, swapped subject/
            // object for `Inverse`). Kleene-star, alternation, etc.
            // are rejected.
            let patterns = expand_path(subject, path, object, cfg)?;
            Ok(Algebra::Bgp { patterns })
        }
        GraphPattern::Join { left, right } => Ok(Algebra::Join {
            left: Box::new(translate_pattern(left, cfg)?),
            right: Box::new(translate_pattern(right, cfg)?),
        }),
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => Ok(Algebra::LeftJoin {
            left: Box::new(translate_pattern(left, cfg)?),
            right: Box::new(translate_pattern(right, cfg)?),
            expr: expression.as_ref().map(translate_expr).transpose()?,
        }),
        GraphPattern::Filter { expr, inner } => Ok(Algebra::Filter {
            expr: translate_expr(expr)?,
            inner: Box::new(translate_pattern(inner, cfg)?),
        }),
        GraphPattern::Union { left, right } => Ok(Algebra::Union {
            left: Box::new(translate_pattern(left, cfg)?),
            right: Box::new(translate_pattern(right, cfg)?),
        }),
        GraphPattern::Project { inner, variables } => Ok(Algebra::Project {
            vars: variables.iter().map(translate_var).collect(),
            inner: Box::new(translate_pattern(inner, cfg)?),
        }),
        GraphPattern::Distinct { inner } => Ok(Algebra::Distinct {
            inner: Box::new(translate_pattern(inner, cfg)?),
        }),
        GraphPattern::Slice {
            inner,
            start,
            length,
        } => Ok(Algebra::Slice {
            inner: Box::new(translate_pattern(inner, cfg)?),
            start: *start,
            length: *length,
        }),
        GraphPattern::OrderBy { inner, expression } => {
            let mut keys = Vec::with_capacity(expression.len());
            for oe in expression {
                let (e, dir) = match oe {
                    OrderExpression::Asc(e) => (translate_expr(e)?, OrderDir::Asc),
                    OrderExpression::Desc(e) => (translate_expr(e)?, OrderDir::Desc),
                };
                keys.push((e, dir));
            }
            Ok(Algebra::OrderBy {
                inner: Box::new(translate_pattern(inner, cfg)?),
                keys,
            })
        }
        GraphPattern::Extend {
            inner,
            variable,
            expression,
        } => Ok(Algebra::Extend {
            inner: Box::new(translate_pattern(inner, cfg)?),
            var: translate_var(variable),
            expr: translate_expr(expression)?,
        }),
        GraphPattern::Values {
            variables,
            bindings,
        } => {
            let vars = variables.iter().map(translate_var).collect();
            let mut rows = Vec::with_capacity(bindings.len());
            for row in bindings {
                let mut out_row = Vec::with_capacity(row.len());
                for cell in row {
                    out_row.push(match cell {
                        Some(gt) => Some(ground_term_to_term(gt)?),
                        None => None,
                    });
                }
                rows.push(out_row);
            }
            Ok(Algebra::Values { vars, rows })
        }
        GraphPattern::Minus { .. } => Err(SparqlError::UnsupportedAlgebra("Minus".into())),
        GraphPattern::Service { .. } => Err(SparqlError::UnsupportedAlgebra("Service".into())),
        GraphPattern::Group { .. } => Err(SparqlError::UnsupportedAlgebra("Group".into())),
        GraphPattern::Reduced { .. } => Err(SparqlError::UnsupportedAlgebra("Reduced".into())),
        GraphPattern::Graph { .. } => Err(SparqlError::UnsupportedAlgebra("Graph".into())),
        GraphPattern::Lateral { .. } => Err(SparqlError::UnsupportedAlgebra("Lateral".into())),
    }
}

fn translate_triple(tp: &SpgTriplePattern, cfg: &SparqlConfig) -> Result<TriplePattern> {
    Ok(TriplePattern {
        subject: term_pattern_to_term(&tp.subject, cfg)?,
        predicate: named_node_pattern_to_term(&tp.predicate)?,
        object: term_pattern_to_term(&tp.object, cfg)?,
    })
}

fn term_pattern_to_term(tp: &TermPattern, cfg: &SparqlConfig) -> Result<Term> {
    Ok(match tp {
        TermPattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        TermPattern::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        TermPattern::Literal(l) => Term::Literal(l.to_string()),
        TermPattern::Variable(v) => Term::Var(translate_var(v)),
        TermPattern::Triple(inner) => {
            if !cfg.rdf12 {
                return Err(SparqlError::UnsupportedAlgebra(
                    "triple-term pattern (enable SparqlConfig::rdf12 to accept)".into(),
                ));
            }
            Term::Triple(Box::new(translate_triple(inner, cfg)?))
        }
    })
}

fn named_node_pattern_to_term(np: &NamedNodePattern) -> Result<Term> {
    Ok(match np {
        NamedNodePattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        NamedNodePattern::Variable(v) => Term::Var(translate_var(v)),
    })
}

fn translate_var(v: &Variable) -> Var {
    Var::new(v.as_str().to_owned())
}

fn ground_term_to_term(gt: &GroundTerm) -> Result<Term> {
    Ok(match gt {
        GroundTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTerm::Literal(l) => Term::Literal(l.to_string()),
        // Ground triple terms appear in `VALUES` rows under SPARQL 1.2.
        // The Stage-1 executor has no in-memory representation for them
        // outside of pattern matching, so we reject them at translation
        // time; relaxing this is part of the SPEC-07 RDF 1.2 follow-up.
        GroundTerm::Triple(_) => {
            return Err(SparqlError::UnsupportedAlgebra(
                "ground triple-term in VALUES (RDF 1.2)".into(),
            ));
        }
    })
}

fn translate_expr(e: &Expression) -> Result<Expr> {
    use Expression as E;
    Ok(match e {
        E::NamedNode(n) => Expr::Term(Term::Iri(n.as_str().to_owned())),
        E::Literal(l) => Expr::Term(Term::Literal(l.to_string())),
        E::Variable(v) => Expr::Term(Term::Var(translate_var(v))),
        E::Equal(a, b) => Expr::Eq(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::SameTerm(a, b) => Expr::Eq(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Less(a, b) => Expr::Lt(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Greater(a, b) => Expr::Gt(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::And(a, b) => Expr::And(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Or(a, b) => Expr::Or(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Not(a) => Expr::Not(Box::new(translate_expr(a)?)),
        E::Bound(v) => Expr::Bound(translate_var(v)),
        other => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "expression: {other:?}"
            )));
        }
    })
}

fn collect_visible_vars(p: &GraphPattern) -> Vec<Var> {
    // SELECT * means "all in-scope vars"; for Stage 1 we walk the
    // pattern once and dedup by name. Order matches first appearance.
    fn push(v: &Variable, acc: &mut Vec<Var>) {
        let new = Var::new(v.as_str().to_owned());
        if !acc.iter().any(|x| x.name() == new.name()) {
            acc.push(new);
        }
    }

    fn walk(p: &GraphPattern, acc: &mut Vec<Var>) {
        match p {
            GraphPattern::Bgp { patterns } => {
                for tp in patterns {
                    if let TermPattern::Variable(v) = &tp.subject {
                        push(v, acc);
                    }
                    if let NamedNodePattern::Variable(v) = &tp.predicate {
                        push(v, acc);
                    }
                    if let TermPattern::Variable(v) = &tp.object {
                        push(v, acc);
                    }
                }
            }
            GraphPattern::Path {
                subject, object, ..
            } => {
                if let TermPattern::Variable(v) = subject {
                    push(v, acc);
                }
                if let TermPattern::Variable(v) = object {
                    push(v, acc);
                }
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::LeftJoin { left, right, .. }
            | GraphPattern::Minus { left, right }
            | GraphPattern::Lateral { left, right } => {
                walk(left, acc);
                walk(right, acc);
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Graph { inner, .. }
            | GraphPattern::Group { inner, .. } => walk(inner, acc),
            GraphPattern::Project { variables, .. } => {
                for v in variables {
                    push(v, acc);
                }
            }
            GraphPattern::Extend {
                inner, variable, ..
            } => {
                walk(inner, acc);
                push(variable, acc);
            }
            GraphPattern::Values { variables, .. } => {
                for v in variables {
                    push(v, acc);
                }
            }
            GraphPattern::Service { .. } => {}
        }
    }

    let mut seen: Vec<Var> = Vec::new();
    walk(p, &mut seen);
    seen
}

/// Expand a (Stage-1 supported) property-path expression into a flat
/// list of triple patterns. Only `Seq` (`/`) and `Inverse` (`^`) and
/// a bare `NamedNode` predicate are supported.
fn expand_path(
    subject: &TermPattern,
    path: &PropertyPathExpression,
    object: &TermPattern,
    cfg: &SparqlConfig,
) -> Result<Vec<TriplePattern>> {
    let mut out = Vec::new();
    let mut fresh = 0usize;
    expand_path_into(subject, path, object, &mut out, &mut fresh, cfg)?;
    Ok(out)
}

fn expand_path_into(
    subject: &TermPattern,
    path: &PropertyPathExpression,
    object: &TermPattern,
    out: &mut Vec<TriplePattern>,
    fresh: &mut usize,
    cfg: &SparqlConfig,
) -> Result<()> {
    use PropertyPathExpression as P;
    match path {
        P::NamedNode(n) => {
            out.push(TriplePattern {
                subject: term_pattern_to_term(subject, cfg)?,
                predicate: Term::Iri(n.as_str().to_owned()),
                object: term_pattern_to_term(object, cfg)?,
            });
            Ok(())
        }
        P::Reverse(inner) => {
            // ^p between s and o == p between o and s
            expand_path_into(object, inner, subject, out, fresh, cfg)
        }
        P::Sequence(a, b) => {
            // (a / b) between s and o introduces a fresh var v with
            // s -a-> v -b-> o
            let mid_name = format!("__path_seq_{}", *fresh);
            *fresh += 1;
            let mid_var = Variable::new(mid_name.clone())
                .map_err(|e| SparqlError::UnsupportedAlgebra(format!("fresh var: {e}")))?;
            let mid_pattern = TermPattern::Variable(mid_var);
            expand_path_into(subject, a, &mid_pattern, out, fresh, cfg)?;
            expand_path_into(&mid_pattern, b, object, out, fresh, cfg)?;
            Ok(())
        }
        other => Err(SparqlError::UnsupportedPathOp(format!("{other:?}"))),
    }
}
