//! High-level convenience: parse → translate → plan → run → return.
//!
//! This is what the HTTP `/query` handler and integration tests use.
//! Callers that need finer control should use the individual modules.

use crate::algebra::translate::translate_query_with;
use crate::error::Result;
use crate::exec::runtime::{construct_triples, describe_triples, Runtime};
use crate::exec::{Bindings, Executor, Store};
use crate::parser::{parse_query, parse_update, ParsedQuery};
use crate::plan::planner;
use crate::update::apply_update;
use crate::SparqlConfig;

/// What `execute_query` returns. Variant chosen by query form.
#[derive(Debug, Clone)]
pub enum QueryAnswer {
    /// SELECT result: list of variable names + solution rows.
    Solutions {
        vars: Vec<String>,
        rows: Vec<Bindings>,
    },
    /// ASK result.
    Boolean(bool),
    /// CONSTRUCT result: ground triples in (s, p, o) lexical form.
    Triples(Vec<(String, String, String)>),
}

pub fn execute_query<E: Executor + ?Sized>(query: &str, exec: &E) -> Result<QueryAnswer> {
    execute_query_with(query, exec, &SparqlConfig::default())
}

/// Like [`execute_query`] but takes an explicit [`SparqlConfig`]; pass
/// `SparqlConfig::rdf12()` to accept RDF 1.2 triple-term patterns.
pub fn execute_query_with<E: Executor + ?Sized>(
    query: &str,
    exec: &E,
    cfg: &SparqlConfig,
) -> Result<QueryAnswer> {
    let parsed = parse_query(query)?;
    match parsed {
        ParsedQuery::Select { inner } => {
            let alg = translate_query_with(&inner, cfg)?;
            let vars = projected_vars(&alg);
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            Ok(QueryAnswer::Solutions { vars, rows })
        }
        ParsedQuery::Ask { inner } => {
            let alg = translate_query_with(&inner, cfg)?;
            let plan = planner::plan(&alg)?;
            let any = Runtime::new(exec).run(&plan)?.next().is_some();
            Ok(QueryAnswer::Boolean(any))
        }
        ParsedQuery::Construct { inner } => {
            let alg = translate_query_with(&inner, cfg)?;
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            let triples = construct_triples(&inner, &rows)?;
            Ok(QueryAnswer::Triples(triples))
        }
        ParsedQuery::Describe { inner } => {
            // DESCRIBE lowers like a SELECT (the projected vars carry the
            // resources to describe); the runtime then expands each bound
            // resource into its forward Concise Bounded Description.
            //
            // SPARQL 1.1 §16.4 also requires describing resources named
            // *directly* by IRI in the DESCRIBE clause, in addition to
            // those found via the WHERE solutions. spargebra lowers such an
            // explicit IRI into `BIND(<iri> AS ?fresh)` layered under the
            // top Project — an `Extend` whose value is a constant IRI. Since
            // `Extend` maps over input rows, it is dropped when the WHERE
            // clause yields zero rows. We therefore seed those explicit IRIs
            // unconditionally (see `explicit_describe_iris`) so they are
            // described even when the WHERE matches nothing.
            let alg = translate_query_with(&inner, cfg)?;
            let seeds = explicit_describe_iris(&alg);
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            let triples = describe_triples(exec, &seeds, &rows)?;
            Ok(QueryAnswer::Triples(triples))
        }
    }
}

pub fn execute_update<S: Store>(update: &str, store: &mut S) -> Result<()> {
    let parsed = parse_update(update)?;
    apply_update(&parsed, store)
}

fn projected_vars(alg: &crate::algebra::Algebra) -> Vec<String> {
    use crate::algebra::Algebra;
    match alg {
        Algebra::Project { vars, .. } => vars.iter().map(|v| v.name().to_owned()).collect(),
        _ => Vec::new(),
    }
}

/// Extract the IRIs named directly in a DESCRIBE clause (SPARQL 1.1
/// §16.4), so they can be described even when the WHERE produces no rows.
///
/// spargebra lowers each explicitly-named DESCRIBE IRI into
/// `BIND(<iri> AS ?fresh)`, i.e. an `Algebra::Extend { expr:
/// Expr::Term(Term::Iri(..)), .. }` stacked under the top-level `Project`.
/// The seed `Extend` chain is not necessarily *directly* under the
/// projection, though: SPARQL solution modifiers nest extra unary nodes
/// between the projection and the seed Extends. The algebra for
/// `DESCRIBE <iri> WHERE {…} ORDER BY … LIMIT …` is roughly
/// `Project{ Slice{ Distinct{ Project{ OrderBy{ Extend…(WHERE) } } } } }`.
/// We therefore walk the *entire* unary projection/modifier spine
/// (`Project` / `Distinct` / `Slice` / `OrderBy` / `Extend`), collecting
/// every constant-IRI `Extend` whose target variable is a describe target,
/// and stop at the first node that is none of those — that node is the
/// WHERE pattern.
///
/// The describe-target variable set is taken *only* from the outer DESCRIBE
/// projection's `vars` and is fixed for the whole walk. Nested/subquery
/// `Project` nodes (e.g. a `{ SELECT ?s ?x WHERE { … } }` subquery in the
/// WHERE clause) do **not** contribute targets: their projected variables
/// (`?x`) must not seed describe IRIs. The translator surfaces the
/// spargebra-generated seed var into the outer projection via
/// `collect_visible_vars`, so `DESCRIBE <iri> … LIMIT/ORDER BY` seeds are
/// still found.
///
/// Caveat (accepted Stage-1 limitation): spargebra erases the distinction
/// between a DESCRIBE-clause IRI and a top-level user
/// `BIND(<iri> AS ?v)`. The `var ∈ targets` gate keeps WHERE-internal user
/// `BIND`s out of the seed set, but a pathological
/// `DESCRIBE ?v WHERE { …matches nothing… BIND(<iri> AS ?v) }` — where the
/// BIND var *is itself* a describe target — will still describe `<iri>`
/// even though strict SELECT semantics would leave `?v` unbound. The common
/// forms — `DESCRIBE <iri>`, `DESCRIBE <iri> WHERE {…}`, and
/// `DESCRIBE ?v WHERE {…}`, with or without `ORDER BY` / `LIMIT` /
/// `OFFSET` / `DISTINCT` — are all handled correctly.
fn explicit_describe_iris(alg: &crate::algebra::Algebra) -> Vec<crate::algebra::Term> {
    use crate::algebra::{Algebra, Expr, Term};
    use std::collections::HashSet;
    let mut iris = Vec::new();
    // DESCRIBE always lowers through a top-level Project; anything else
    // carries no explicit-IRI seeds.
    let Algebra::Project { vars, inner } = alg else {
        return iris;
    };
    // Targets are exactly the outer DESCRIBE projection's variables, fixed
    // for the whole walk. Nested/subquery `Project` nodes must not expand
    // this set, or a subquery's own projected vars would wrongly seed IRIs.
    let targets: HashSet<String> = vars.iter().map(|v| v.name().to_owned()).collect();
    let mut node = inner.as_ref();
    loop {
        match node {
            Algebra::Project { inner, .. } => node = inner.as_ref(),
            Algebra::Distinct { inner } => node = inner.as_ref(),
            Algebra::Slice { inner, .. } => node = inner.as_ref(),
            Algebra::OrderBy { inner, .. } => node = inner.as_ref(),
            Algebra::Extend { inner, var, expr } => {
                if let Expr::Term(Term::Iri(s)) = expr {
                    if targets.contains(var.name()) {
                        iris.push(Term::Iri(s.clone()));
                    }
                }
                node = inner.as_ref();
            }
            // Any other variant (Bgp/Join/LeftJoin/Filter/Union/Values):
            // the WHERE-pattern boundary. Stop walking.
            _ => break,
        }
    }
    iris
}
