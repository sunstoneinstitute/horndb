//! High-level convenience: parse → translate → plan → run → return.
//!
//! This is what the HTTP `/query` handler and integration tests use.
//! Callers that need finer control should use the individual modules.

use crate::algebra::translate::translate_query_with;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::{construct_triples, describe_triples, Runtime};
use crate::exec::{Bindings, Executor, FullBackend};
use crate::parser::{parse_query, parse_update, ParsedQuery};
use crate::plan::explain::{explain, ExecutionMode, ExplainFormat};
use crate::plan::planner;
use crate::update::apply_update_with;
use crate::SparqlConfig;
use horndb_metrics::labels::{QueryKind, QueryKindLabel, Stage, StageLabel};
use std::time::Instant;

/// Time the closure as a single pipeline `stage`, recording its wall-clock
/// duration in `stage_duration_seconds` and, on `Err`, bumping the
/// `query_errors` counter for that stage. Behaviour-preserving: the closure's
/// `Result` is returned verbatim so `?` propagation is unchanged. Only
/// whole-stage timing is recorded here — never per-tuple/per-row work.
fn timed<T>(stage: Stage, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let m = horndb_metrics::metrics();
    let start = Instant::now();
    let out = f();
    let label = StageLabel { stage };
    m.sparql
        .stage_duration_seconds
        .get_or_create(&label)
        .observe(start.elapsed().as_secs_f64());
    if out.is_err() {
        m.sparql.query_errors.get_or_create(&label).inc();
    }
    out
}

/// Classify a parsed query into its metric `QueryKind`. `EXPLAIN` is reported
/// as the kind of the query it wraps.
fn classify_kind(parsed: &ParsedQuery) -> QueryKind {
    match parsed {
        ParsedQuery::Select { .. } => QueryKind::Select,
        ParsedQuery::Ask { .. } => QueryKind::Ask,
        ParsedQuery::Construct { .. } => QueryKind::Construct,
        ParsedQuery::Describe { .. } => QueryKind::Describe,
        ParsedQuery::Explain { inner, .. } => classify_kind(inner),
    }
}

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
    /// `EXPLAIN` result (SPEC-07 F9): the rendered physical plan with
    /// execution mode and cardinality estimates. The query is **not**
    /// executed. `json` records the rendering format so the HTTP layer
    /// can pick the right content type.
    Explanation { text: String, json: bool },
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
    let parsed = timed(Stage::Parse, || parse_query(query))?;
    horndb_metrics::metrics()
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel {
            kind: classify_kind(&parsed),
        })
        .inc();
    match parsed {
        ParsedQuery::Select { inner } => {
            let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
            let vars = projected_vars(&alg);
            let plan = timed(Stage::Plan, || planner::plan(&alg))?;
            let rows: Vec<Bindings> = timed(Stage::Exec, || {
                Runtime::new(exec).run(&plan).map(Iterator::collect)
            })?;
            Ok(QueryAnswer::Solutions { vars, rows })
        }
        ParsedQuery::Ask { inner } => {
            let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
            let plan = timed(Stage::Plan, || planner::plan(&alg))?;
            let any = timed(Stage::Exec, || {
                // Early exit: only the first operator chunk is pulled and
                // decoded — `run` would drain the whole result set.
                let rt = Runtime::new(exec);
                let mut stream = rt.run_stream(&plan)?;
                Ok(stream.next_chunk()?.is_some())
            })?;
            Ok(QueryAnswer::Boolean(any))
        }
        ParsedQuery::Construct { inner } => {
            let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
            let plan = timed(Stage::Plan, || planner::plan(&alg))?;
            let rows: Vec<Bindings> = timed(Stage::Exec, || {
                Runtime::new(exec).run(&plan).map(Iterator::collect)
            })?;
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
            let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
            let seeds = explicit_describe_iris(&alg);
            let plan = timed(Stage::Plan, || planner::plan(&alg))?;
            let rows: Vec<Bindings> = timed(Stage::Exec, || {
                Runtime::new(exec).run(&plan).map(Iterator::collect)
            })?;
            let triples = describe_triples(exec, &seeds, &rows)?;
            Ok(QueryAnswer::Triples(triples))
        }
        ParsedQuery::Explain { inner, json } => {
            // EXPLAIN does not run the query: translate + plan only, then
            // render. The execution mode is the entailment regime;
            // backward-chaining (#55) is not yet selectable, so it is
            // always `Materialized` today (the renderer labels that).
            // `plan_of` fuses translate + plan and EXPLAIN never executes, so
            // record a single `Plan` stage (no `Exec`).
            let plan = timed(Stage::Plan, || plan_of(&inner, cfg))?;
            let format = if json {
                ExplainFormat::Json
            } else {
                ExplainFormat::Text
            };
            let text = explain(&plan, exec, ExecutionMode::Materialized, format);
            Ok(QueryAnswer::Explanation { text, json })
        }
    }
}

/// Parse → translate → plan a query for streaming execution, without
/// running it. Returns `Some((projected_vars, plan))` for a plain SELECT;
/// `None` for every other form (ASK / CONSTRUCT / DESCRIBE / EXPLAIN),
/// which the caller answers via [`execute_query`]. Records the same
/// Parse/Translate/Plan stage metrics as `execute_query`;
/// `query_total{kind=select}` is bumped only on the `Some` path so the
/// fallback keeps per-kind counts exact (a non-SELECT query costs one
/// extra `parse` stage observation from the routing double-parse — noted
/// in `docs/metrics.md`).
pub fn plan_select(
    query: &str,
    cfg: &SparqlConfig,
) -> Result<Option<(Vec<String>, crate::plan::PhysicalPlan)>> {
    let parsed = timed(Stage::Parse, || parse_query(query))?;
    let ParsedQuery::Select { inner } = parsed else {
        return Ok(None);
    };
    horndb_metrics::metrics()
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel {
            kind: QueryKind::Select,
        })
        .inc();
    let alg = timed(Stage::Translate, || translate_query_with(&inner, cfg))?;
    let vars = projected_vars(&alg);
    let plan = timed(Stage::Plan, || planner::plan(&alg))?;
    Ok(Some((vars, plan)))
}

/// Translate + plan a (non-EXPLAIN) parsed query into its physical plan,
/// without executing it. Shared by the `EXPLAIN` path. Nested `EXPLAIN`
/// (`EXPLAIN EXPLAIN …`) is rejected — it is not meaningful.
fn plan_of(parsed: &ParsedQuery, cfg: &SparqlConfig) -> Result<crate::plan::PhysicalPlan> {
    let inner = match parsed {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner }
        | ParsedQuery::Describe { inner } => inner,
        ParsedQuery::Explain { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "nested EXPLAIN is not supported".into(),
            ));
        }
    };
    let alg = translate_query_with(inner, cfg)?;
    planner::plan(&alg)
}

pub fn execute_update<B: FullBackend>(update: &str, store: &mut B) -> Result<()> {
    execute_update_with(update, store, &SparqlConfig::default())
}

/// Like [`execute_update`] but takes an explicit [`SparqlConfig`].
pub fn execute_update_with<B: FullBackend>(
    update: &str,
    store: &mut B,
    cfg: &SparqlConfig,
) -> Result<()> {
    let parsed = timed(Stage::Parse, || parse_update(update))?;
    horndb_metrics::metrics()
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel {
            kind: QueryKind::Update,
        })
        .inc();
    timed(Stage::Exec, || apply_update_with(&parsed, store, cfg))
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
/// Caveat (accepted Stage-1 limitation — irreducible at the algebra level):
/// spargebra lowers an explicitly-named DESCRIBE IRI to `BIND(<iri> AS ?f)`
/// using a *random, freshly-generated* variable name
/// (`Variable::new_unchecked(format!("{:x}", random::<u128>()))`), which is
/// structurally indistinguishable from a user-written `BIND(<iri> AS ?v)`.
/// The original DESCRIBE target list is not preserved on `Query::Describe`,
/// so the two cannot be told apart from the translated algebra. The
/// `var ∈ targets` gate keeps ordinary WHERE-internal user `BIND`s (whose
/// var is not a describe target) out of the seed set, which covers the
/// realistic cases. The residual gap is a *contrived* query whose describe
/// target variable is itself bound by a user `BIND` to a constant IRI over a
/// pattern that matches nothing — directly or inside a subquery, e.g.
/// `DESCRIBE ?v WHERE { …no match… BIND(<iri> AS ?v) }` or
/// `DESCRIBE ?v WHERE { { SELECT ?v WHERE { …no match… BIND(<iri> AS ?v) } } }`.
/// Such a query still describes `<iri>` even though strict SELECT semantics
/// would leave `?v` unbound. Because DESCRIBE result graphs are
/// implementation-defined (SPARQL 1.1 §16.4) and the over-described resource
/// is exactly the IRI named in the query text, this is a benign
/// over-approximation, not a wrong-data defect. The common forms —
/// `DESCRIBE <iri>`, `DESCRIBE <iri> WHERE {…}`, and `DESCRIBE ?v WHERE {…}`,
/// with or without `ORDER BY` / `LIMIT` / `OFFSET` / `DISTINCT` and with
/// subqueries that do not rebind a describe target — are all handled
/// correctly.
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
