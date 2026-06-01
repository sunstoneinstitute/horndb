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
            let alg = translate_query_with(&inner, cfg)?;
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            let triples = describe_triples(exec, &rows)?;
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
