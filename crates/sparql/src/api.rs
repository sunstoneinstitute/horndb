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
/// Expr::Term(Term::Iri(..)), .. }` stacked directly under the top-level
/// `Project`. We peel that constant-IRI Extend chain off the Project's
/// `inner`, collecting each IRI, and stop at the first node that is not
/// such an Extend — that node is the WHERE pattern.
///
/// Caveat (accepted Stage-1 limitation): spargebra erases the distinction
/// between a DESCRIBE-clause IRI and a top-level user
/// `BIND(<iri> AS ?v)`. A pathological `DESCRIBE ?v WHERE { …matches
/// nothing… BIND(<iri> AS ?v) }` will therefore describe `<iri>` even
/// though strict SELECT semantics would leave `?v` unbound. The common
/// forms — `DESCRIBE <iri>`, `DESCRIBE <iri> WHERE {…}`, and
/// `DESCRIBE ?v WHERE {…}` — are all handled correctly.
fn explicit_describe_iris(alg: &crate::algebra::Algebra) -> Vec<crate::algebra::Term> {
    use crate::algebra::{Algebra, Expr, Term};
    let mut iris = Vec::new();
    let mut node = match alg {
        Algebra::Project { inner, .. } => inner.as_ref(),
        // DESCRIBE always lowers through a top-level Project; anything
        // else carries no explicit-IRI seeds.
        _ => return iris,
    };
    while let Algebra::Extend { inner, expr, .. } = node {
        match expr {
            Expr::Term(Term::Iri(s)) => {
                iris.push(Term::Iri(s.clone()));
                node = inner.as_ref();
            }
            // First non-constant-IRI Extend (or any other node): this is
            // the WHERE pattern. Stop peeling.
            _ => break,
        }
    }
    iris
}
