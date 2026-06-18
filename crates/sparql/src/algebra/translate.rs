//! Algebra translation from `spargebra` AST to our internal [`Algebra`].
//!
//! Stage 1 supports a deliberately small operator set; constructs we
//! do not yet handle return `SparqlError::UnsupportedAlgebra` (or
//! `UnsupportedPathOp` for the recursive `*`/`+` property paths) so the
//! planner never has to defend against them. The non-recursive path
//! operators (`/`, `^`, `|`, `?`, `!`) are lowered in
//! [`translate_path`].

use crate::algebra::{AggFunc, Aggregate, Algebra, Expr, Func, OrderDir, Term, TriplePattern, Var};
use crate::error::{Result, SparqlError};
use crate::SparqlConfig;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
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
        } => translate_projection(pattern, cfg),
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
        Query::Describe {
            pattern,
            dataset: _,
            base_iri: _,
        } => {
            // spargebra encodes a DESCRIBE's WHERE clause exactly like a
            // SELECT (via `build_select`): the resources to describe are
            // the values bound to the projected variables across all
            // result rows. So the algebra translation is identical to
            // the SELECT arm — the runtime (`describe_triples`) is what
            // turns those bound resources into a forward CBD graph.
            translate_projection(pattern, cfg)
        }
    }
}

/// Shared SELECT/DESCRIBE lowering: spargebra often wraps the WHERE
/// clause in a `Project` node already (for the projected variable
/// list). If so, honour it; otherwise wrap ourselves with the
/// visible-var list.
fn translate_projection(pattern: &GraphPattern, cfg: &SparqlConfig) -> Result<Algebra> {
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

/// Lower a bare WHERE `GraphPattern` (as carried by `DELETE/INSERT … WHERE`
/// updates) to our `Algebra`. Unlike [`translate_query_with`], there is no
/// surrounding query form / projection — the caller plans and runs it to
/// obtain the solution rows that instantiate the update templates.
pub(crate) fn translate_where(p: &GraphPattern, cfg: &SparqlConfig) -> Result<Algebra> {
    translate_pattern(p, cfg)
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
            // Non-recursive property paths lower at translation time:
            //   `/` (Seq) and `^` (Inverse) expand into triple patterns
            //   (fresh intermediate var for Seq, swapped subject/object
            //   for Inverse); `|` (Alternative) and `?` (ZeroOrOne) lower
            //   to `Union`; `!` (NegatedPropertySet) lowers to a wildcard
            //   predicate filtered by `NOT IN {…}`. Kleene `*`/`+`
            //   (recursive, route to closure) are still rejected.
            //
            // A single property-path expression is *set*-valued: it
            // matches each (start, end) pair at most once, regardless of
            // how many distinct routes connect them (alternative branches,
            // unexcluded predicates of `!`, the `?` zero/one overlap). The
            // lowering can emit a route per witness, and those witnesses
            // differ only in the *hidden* variables it mints (the `!`
            // predicate slot, sequence join nodes). So we first `Project`
            // the result down to the user-visible endpoint variables —
            // dropping the hidden ones — then `Distinct` to collapse the
            // duplicate routes before the rows escape the path.
            let s = match_term(subject, cfg)?;
            let o = match_term(object, cfg)?;
            let visible = visible_path_vars(&s, &o);
            let inner = translate_path(s, path, o)?;
            if visible.is_empty() {
                // Both endpoints are ground (or hidden): the path is a pure
                // existence test. Collapse any number of matching routes to
                // at most one empty solution. (`Project { vars: [] }` can't
                // express this — the runtime reads empty projection as
                // `SELECT *` and would preserve the hidden witness columns,
                // defeating the de-duplication.)
                Ok(Algebra::Slice {
                    inner: Box::new(inner),
                    start: 0,
                    length: Some(1),
                })
            } else {
                // Project to the visible endpoint variables (dropping the
                // hidden witness columns), then `Distinct` so each
                // (start, end) pair survives at most once.
                Ok(Algebra::Distinct {
                    inner: Box::new(Algebra::Project {
                        vars: visible,
                        inner: Box::new(inner),
                    }),
                })
            }
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
        GraphPattern::Group {
            inner,
            variables,
            aggregates,
        } => {
            let keys = variables.iter().map(translate_var).collect();
            let mut aggs = Vec::with_capacity(aggregates.len());
            for (out_var, agg_expr) in aggregates {
                aggs.push(translate_aggregate(out_var, agg_expr)?);
            }
            Ok(Algebra::Group {
                inner: Box::new(translate_pattern(inner, cfg)?),
                keys,
                aggregates: aggs,
            })
        }
        GraphPattern::Minus { .. } => Err(SparqlError::UnsupportedAlgebra("Minus".into())),
        GraphPattern::Service { .. } => Err(SparqlError::UnsupportedAlgebra("Service".into())),
        GraphPattern::Reduced { .. } => Err(SparqlError::UnsupportedAlgebra("Reduced".into())),
        // Stage-1 merged-graph semantics: the executor holds a single
        // graph (SPB/W3C corpora are loaded from flat dumps), so
        // `GRAPH <iri> { P }` and `GRAPH ?g { P }` lower to `P`. A
        // graph-name variable stays unbound in the results. True
        // named-graph scoping arrives with the storage wiring (#67) —
        // see INTEGRATION-NOTES.md.
        GraphPattern::Graph { name: _, inner } => translate_pattern(inner, cfg),
        GraphPattern::Lateral { .. } => Err(SparqlError::UnsupportedAlgebra("Lateral".into())),
    }
}

fn translate_triple(tp: &SpgTriplePattern, cfg: &SparqlConfig) -> Result<TriplePattern> {
    // Blank nodes in a WHERE graph pattern are non-distinguished
    // variables (SPARQL 1.1 §4.1.4), so the subject/object positions go
    // through `match_term`, which maps them to join variables. The
    // predicate position cannot be a blank node.
    Ok(TriplePattern {
        subject: match_term(&tp.subject, cfg)?,
        predicate: named_node_pattern_to_term(&tp.predicate)?,
        object: match_term(&tp.object, cfg)?,
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

fn translate_aggregate(out_var: &Variable, agg: &AggregateExpression) -> Result<Aggregate> {
    let out = translate_var(out_var);
    Ok(match agg {
        AggregateExpression::CountSolutions { distinct } => Aggregate {
            out,
            func: AggFunc::CountStar,
            distinct: *distinct,
        },
        AggregateExpression::FunctionCall {
            name,
            expr,
            distinct,
        } => {
            let e = Box::new(translate_expr(expr)?);
            let func = match name {
                AggregateFunction::Count => AggFunc::Count(e),
                AggregateFunction::Sum => AggFunc::Sum(e),
                AggregateFunction::Min => AggFunc::Min(e),
                AggregateFunction::Max => AggFunc::Max(e),
                AggregateFunction::Avg => AggFunc::Avg(e),
                AggregateFunction::Sample => AggFunc::Sample(e),
                AggregateFunction::GroupConcat { separator } => AggFunc::GroupConcat {
                    expr: e,
                    separator: separator.clone().unwrap_or_else(|| " ".to_string()),
                },
                AggregateFunction::Custom(n) => {
                    return Err(SparqlError::UnsupportedAlgebra(format!(
                        "custom aggregate {}",
                        n.as_str()
                    )));
                }
            };
            Aggregate {
                out,
                func,
                distinct: *distinct,
            }
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
        E::LessOrEqual(a, b) => {
            Expr::Le(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::GreaterOrEqual(a, b) => {
            Expr::Ge(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::In(a, list) => {
            let head = Box::new(translate_expr(a)?);
            let items = list
                .iter()
                .map(translate_expr)
                .collect::<Result<Vec<_>>>()?;
            Expr::In(head, items)
        }
        E::And(a, b) => Expr::And(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Or(a, b) => Expr::Or(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Not(a) => Expr::Not(Box::new(translate_expr(a)?)),
        E::Bound(v) => Expr::Bound(translate_var(v)),
        E::Add(a, b) => Expr::Add(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Subtract(a, b) => Expr::Sub(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Multiply(a, b) => Expr::Mul(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Divide(a, b) => Expr::Div(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::UnaryPlus(a) => translate_expr(a)?,
        E::UnaryMinus(a) => Expr::Neg(Box::new(translate_expr(a)?)),
        E::If(c, t, f) => Expr::If(
            Box::new(translate_expr(c)?),
            Box::new(translate_expr(t)?),
            Box::new(translate_expr(f)?),
        ),
        E::Coalesce(args) => Expr::Coalesce(
            args.iter()
                .map(translate_expr)
                .collect::<Result<Vec<_>>>()?,
        ),
        E::FunctionCall(func, args) => {
            let f = translate_function(func)?;
            Expr::Func(
                f,
                args.iter()
                    .map(translate_expr)
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        other => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "expression: {other:?}"
            )));
        }
    })
}

/// Map a spargebra builtin to the Stage-1 [`Func`] set. Functions
/// outside the set (non-deterministic, hashing, SPARQL 1.2 triple
/// accessors, custom IRIs) are rejected here so the planner and
/// runtime never see them.
fn translate_function(f: &Function) -> Result<Func> {
    Ok(match f {
        Function::Str => Func::Str,
        Function::Lang => Func::Lang,
        Function::LangMatches => Func::LangMatches,
        Function::Datatype => Func::Datatype,
        Function::StrLen => Func::StrLen,
        Function::SubStr => Func::SubStr,
        Function::UCase => Func::UCase,
        Function::LCase => Func::LCase,
        Function::StrStarts => Func::StrStarts,
        Function::StrEnds => Func::StrEnds,
        Function::Contains => Func::Contains,
        Function::StrBefore => Func::StrBefore,
        Function::StrAfter => Func::StrAfter,
        Function::Concat => Func::Concat,
        Function::Replace => Func::Replace,
        Function::Regex => Func::Regex,
        Function::Abs => Func::Abs,
        Function::Ceil => Func::Ceil,
        Function::Floor => Func::Floor,
        Function::Round => Func::Round,
        Function::IsIri => Func::IsIri,
        Function::IsBlank => Func::IsBlank,
        Function::IsLiteral => Func::IsLiteral,
        Function::IsNumeric => Func::IsNumeric,
        Function::Year => Func::Year,
        Function::Month => Func::Month,
        Function::Day => Func::Day,
        Function::Hours => Func::Hours,
        Function::Minutes => Func::Minutes,
        Function::Seconds => Func::Seconds,
        other => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "function: {other:?}"
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
            | GraphPattern::Group { inner, .. } => walk(inner, acc),
            GraphPattern::Graph { name, inner } => {
                // The graph-name variable is in scope (and projected by
                // `SELECT *`) even though the Stage-1 merged-graph
                // lowering never binds it.
                if let NamedNodePattern::Variable(v) = name {
                    push(v, acc);
                }
                walk(inner, acc);
            }
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

/// Lower a non-recursive property-path expression between `subject` and
/// `object` to an [`Algebra`] subtree. Supported operators:
///
/// * bare `NamedNode` predicate, `^` (Inverse), `/` (Sequence) — expand
///   to one or more triple patterns (a `Bgp`, joined for sub-paths that
///   are not themselves BGPs);
/// * `|` (Alternative) and `?` (ZeroOrOne) — `Union`;
/// * `!` (NegatedPropertySet) — a wildcard-predicate `Bgp` wrapped in a
///   `Filter` that excludes the listed predicates.
///
/// Kleene `*`/`+` are recursive (route to closure) and remain rejected
/// with [`SparqlError::UnsupportedPathOp`].
///
/// Endpoints arrive as already-lowered [`Term`]s (the caller runs them
/// through [`match_term`]). Carrying `Term` rather than `spargebra`'s
/// `TermPattern` lets the `Sequence` arm mint its intermediate join node
/// as a `Term::Var` with a user-unspellable name directly — `spargebra`'s
/// `Variable::new` would reject that name. Intermediate names come from
/// [`fresh_path_var`] (a process-global counter), so two distinct path
/// patterns never accidentally join on a reused hidden variable.
///
/// Multiplicity is *not* the concern of this function: a single path may
/// legitimately emit several witnesses for one (start, end) pair; the
/// caller wraps the whole result in `Distinct` to make the path
/// set-valued.
fn translate_path(subject: Term, path: &PropertyPathExpression, object: Term) -> Result<Algebra> {
    use PropertyPathExpression as P;
    match path {
        P::NamedNode(n) => Ok(Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject,
                predicate: Term::Iri(n.as_str().to_owned()),
                object,
            }],
        }),
        P::Reverse(inner) => {
            // `^p` between s and o == `p` between o and s.
            translate_path(object, inner, subject)
        }
        P::Sequence(a, b) => {
            // `(a / b)` between s and o introduces a fresh var v with
            // `s -a-> v -b-> o`. Each side lowers independently; if both
            // are plain BGPs we merge their patterns, otherwise we `Join`.
            let mid = Term::Var(Var::new(fresh_path_var("seq")));
            let left = translate_path(subject, a, mid.clone())?;
            let right = translate_path(mid, b, object)?;
            Ok(join_algebra(left, right))
        }
        P::Alternative(a, b) => {
            // `(a | b)` between s and o is the union of the two paths.
            let left = translate_path(subject.clone(), a, object.clone())?;
            let right = translate_path(subject, b, object)?;
            Ok(Algebra::Union {
                left: Box::new(left),
                right: Box::new(right),
            })
        }
        P::ZeroOrOne(inner) => {
            // `p?` between s and o is the union of the zero-length path
            // (s and o denote the same node) with the single `p` step.
            let zero = zero_length_path(subject.clone(), object.clone())?;
            let one = translate_path(subject, inner, object)?;
            Ok(Algebra::Union {
                left: Box::new(zero),
                right: Box::new(one),
            })
        }
        P::NegatedPropertySet(preds) => {
            // `!(p1|…|pn)` between s and o: match `s ?p o` with a fresh
            // predicate variable, then filter out the excluded set. Note
            // spargebra carries only forward predicates here; an inverse
            // member `^p` is parsed as `Reverse(NegatedPropertySet([p]))`
            // and handled by the `Reverse` arm above.
            let pred_var = Var::new(fresh_path_var("neg"));
            let bgp = Algebra::Bgp {
                patterns: vec![TriplePattern {
                    subject,
                    predicate: Term::Var(pred_var.clone()),
                    object,
                }],
            };
            if preds.is_empty() {
                // `!()` excludes nothing — every predicate matches.
                return Ok(bgp);
            }
            let lhs = Expr::Term(Term::Var(pred_var));
            let list = preds
                .iter()
                .map(|n| Expr::Term(Term::Iri(n.as_str().to_owned())))
                .collect();
            Ok(Algebra::Filter {
                expr: Expr::Not(Box::new(Expr::In(Box::new(lhs), list))),
                inner: Box::new(bgp),
            })
        }
        other => Err(SparqlError::UnsupportedPathOp(format!("{other:?}"))),
    }
}

/// Combine two property-path sub-algebras. When both are plain BGPs we
/// concatenate their patterns (the common case — keeps `/` and `^`
/// chains in a single scan-friendly BGP); otherwise we `Join` them so
/// `Union`/`Filter` sub-paths compose correctly.
fn join_algebra(left: Algebra, right: Algebra) -> Algebra {
    match (left, right) {
        (Algebra::Bgp { patterns: mut l }, Algebra::Bgp { patterns: r }) => {
            l.extend(r);
            Algebra::Bgp { patterns: l }
        }
        (l, r) => Algebra::Join {
            left: Box::new(l),
            right: Box::new(r),
        },
    }
}

/// The zero-length path between `subject` and `object` — they must denote
/// the same node. Lowers without enumerating the graph:
///
/// * both ground & equal → a single empty solution; both ground & unequal
///   → no solution;
/// * one variable, one ground → bind the variable to the ground term.
///
/// The genuinely unbounded cases — two *distinct* variables, **or the same
/// variable on both ends** (`?x p? ?x`) — are rejected. Both would have to
/// range the variable over every node in the graph (a zero-length path
/// binds `?x` to each node, not to an unbound row), which is out of
/// Stage-1 scope; they belong with the recursive `*`/`+` increment that
/// routes through closure.
///
/// Endpoints arrive already lowered (the caller runs them through
/// [`match_term`]), so a blank-node endpoint minted by spargebra's
/// sequence flattening is treated as a (join) variable, just like in the
/// single-step arms.
fn zero_length_path(subject: Term, object: Term) -> Result<Algebra> {
    // A single empty solution (the identity / unit relation).
    let unit = Algebra::Values {
        vars: Vec::new(),
        rows: vec![Vec::new()],
    };
    // No solution.
    let empty = Algebra::Values {
        vars: Vec::new(),
        rows: Vec::new(),
    };
    match (subject, object) {
        (Term::Var(_), Term::Var(_)) => {
            // Two variable endpoints (whether the same variable, `?x p? ?x`,
            // or two distinct ones) require binding a variable to every node
            // in the graph for the zero-length branch — out of Stage-1 scope.
            // Returning the unit relation here would emit an *unbound* row
            // instead of the per-node bindings, which is wrong, so reject.
            Err(SparqlError::UnsupportedPathOp(
                "zero-or-one path `?` with an unbound variable on both ends \
                 (would enumerate every node) — out of Stage-1 scope"
                    .into(),
            ))
        }
        (Term::Var(v), other) | (other, Term::Var(v)) => Ok(Algebra::Values {
            vars: vec![v],
            rows: vec![vec![Some(other)]],
        }),
        // Both ground: equal iff their term forms match.
        (st, ot) => Ok(if st == ot { unit } else { empty }),
    }
}

/// Lower a term that appears in a *matching* position of a WHERE graph
/// pattern (triple-pattern or property-path endpoint). Identical to
/// [`term_pattern_to_term`] except that a **blank node** is mapped to a
/// (deterministically named) variable rather than a constant.
///
/// A blank node in a query pattern is a non-distinguished variable
/// (SPARQL 1.1 §4.1.4): two occurrences of the same label must co-refer
/// and join, but it can never match a *specific* blank node in the data.
/// spargebra also relies on this when it flattens a path sequence
/// `s p1/p2 o` into two joined patterns connected by a freshly minted
/// blank node — that node must act as a join key across the resulting
/// algebra. Lowering it to a constant (as the generic term lowering does)
/// would match nothing.
fn match_term(tp: &TermPattern, cfg: &SparqlConfig) -> Result<Term> {
    match tp {
        TermPattern::BlankNode(b) => Ok(Term::Var(Var::new(hidden_var_name(
            "bnode",
            Some(b.as_str()),
        )))),
        other => term_pattern_to_term(other, cfg),
    }
}

/// Prefix for the internal variables minted during path/blank-node
/// lowering. It begins with `?`, which the SPARQL `VARNAME` grammar can
/// never produce (a parsed variable's stored name carries no sigil and
/// cannot contain `?`), so these synthetic names are **impossible to
/// collide with any user variable** — they can be neither written in a
/// `SELECT` list nor matched against a user binding.
const HIDDEN_VAR_PREFIX: &str = "?pp";

/// Whether `v` is an internal path/blank-node variable (see
/// [`HIDDEN_VAR_PREFIX`]) rather than a user-visible one.
fn is_hidden_var(v: &Var) -> bool {
    v.name().starts_with(HIDDEN_VAR_PREFIX)
}

/// The user-visible variables among a path's two (already-lowered)
/// endpoints — i.e. real query variables, excluding the hidden
/// existentials minted for blank-node endpoints. Order-preserving and
/// de-duplicated (a path like `?x p ?x` exposes `?x` once). These are the
/// only columns a single property-path expression may bind, so projecting
/// to them before `Distinct` drops the internal witness columns that
/// would otherwise defeat set-valued de-duplication.
fn visible_path_vars(s: &Term, o: &Term) -> Vec<Var> {
    let mut out = Vec::new();
    for t in [s, o] {
        if let Term::Var(v) = t {
            if !is_hidden_var(v) && !out.contains(v) {
                out.push(v.clone());
            }
        }
    }
    out
}

/// Build an internal (user-unspellable) variable name. `kind` tags the
/// role (`bnode`, `seq`, `neg`); `tag` is an optional stable suffix (the
/// blank-node label) used when the name must be *deterministic* so two
/// occurrences co-refer; pass `None` to draw a globally fresh counter
/// value instead.
fn hidden_var_name(kind: &str, tag: Option<&str>) -> String {
    match tag {
        Some(t) => format!("{HIDDEN_VAR_PREFIX}_{kind}_{t}"),
        None => {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("{HIDDEN_VAR_PREFIX}_{kind}_{n}")
        }
    }
}

/// Mint a hidden variable name unique across the whole translated query
/// (process-global counter). Two distinct path patterns in one query
/// (e.g. two `!` sets, or two `/` sequences) must not collide on a hidden
/// name, or the join would force their unrelated hidden bindings to be
/// equal and silently drop rows. The names are user-unspellable
/// ([`HIDDEN_VAR_PREFIX`]) and are projected away before results return.
fn fresh_path_var(kind: &str) -> String {
    hidden_var_name(kind, None)
}
