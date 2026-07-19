//! SPEC-23 Phase 2 slot-differential suite: the four heuristic rewrite
//! passes must not change any result. For every query we compare the FULL
//! pipeline against the pipeline with EACH pass singly disabled — identical
//! result multisets prove (a) result-invariance and (b) that a future
//! regression bisects to exactly one `PassId`.

use horndb_sparql::algebra::translate::translate_query_with;
use horndb_sparql::algebra::{Algebra, Term};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::{Bindings, Store};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::pass::{PassId, PlanCtx};
use horndb_sparql::plan::planner;
use horndb_sparql::SparqlConfig;
use std::collections::HashSet;

const PHASE2_PASSES: [PassId; 4] = [
    PassId::Normalize,
    PassId::FilterPullup,
    PassId::FilterPushdown,
    PassId::ProjectionPushdown,
];

fn algebra_of(q: &str) -> Algebra {
    let inner = match parse_query(q).expect("parse") {
        ParsedQuery::Select { inner } => inner,
        other => panic!("expected SELECT, got {other:?}"),
    };
    translate_query_with(&inner, &SparqlConfig::default()).expect("translate")
}

/// A small fixture: a knows b, b knows c, c knows a (a 3-cycle), plus d who
/// shares Alice's name with a but has no age and knows nobody. Gives every
/// query in the battery a mix of matching, non-matching, and OPTIONAL-only
/// rows to differentiate on.
fn fixture() -> HornBackend {
    let mut horn = HornBackend::new();
    let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
    let lit = |s: &str| Term::Literal(format!("\"{s}\""));

    horn.insert_triple(iri("a"), iri("name"), lit("Alice"));
    horn.insert_triple(iri("a"), iri("age"), lit("30"));
    horn.insert_triple(iri("a"), iri("knows"), iri("b"));

    horn.insert_triple(iri("b"), iri("name"), lit("Bob"));
    horn.insert_triple(iri("b"), iri("age"), lit("25"));
    horn.insert_triple(iri("b"), iri("knows"), iri("c"));

    horn.insert_triple(iri("c"), iri("name"), lit("Carol"));
    horn.insert_triple(iri("c"), iri("knows"), iri("a"));

    horn.insert_triple(iri("d"), iri("name"), lit("Alice"));

    horn
}

/// Order-independent multiset rendering of a result set.
fn canon(rows: Vec<Bindings>) -> Vec<String> {
    let mut v: Vec<String> = rows
        .into_iter()
        .map(|b| {
            b.vars()
                .map(|(k, t)| format!("{k}={t:?}"))
                .collect::<Vec<_>>()
                .join("\u{1}")
        })
        .collect();
    v.sort();
    v
}

fn run_with(horn: &HornBackend, query: &str, disabled: HashSet<PassId>) -> Vec<String> {
    let alg = algebra_of(query);
    let ctx = PlanCtx {
        disabled_passes: disabled,
    };
    let plan = planner::plan_with_ctx(&alg, &ctx).expect("plan");
    let rows: Vec<Bindings> = Runtime::new(horn).run(&plan).expect("run").collect();
    canon(rows)
}

const QUERIES: &[&str] = &[
    "SELECT * WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s ?p ?o }",
    "SELECT ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age > \"20\") }",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n FILTER(?o = <http://ex/b>) }",
    "SELECT ?s WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
    "SELECT ?s ?age WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age FILTER(?age > \"20\") } }",
    "SELECT ?x WHERE { { ?x <http://ex/name> ?n } UNION { ?x <http://ex/age> ?a } }",
    "SELECT DISTINCT ?n WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age } ORDER BY ?age",
    "SELECT (COUNT(*) AS ?c) WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?n (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
    // OPTIONAL-side filter: FilterPushdown must NOT sink it into the optional arm.
    "SELECT ?s ?age WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } FILTER(?age > \"10\" || !BOUND(?age)) }",
    // FilterPullup unbound-arm guard: filter in a group over a var bound only outside it.
    "SELECT * WHERE { { ?s <http://ex/name> ?n FILTER(?age > \"0\") } ?s <http://ex/age> ?age }",
    // FilterPushdown Project scope-hiding: filter above a subquery over a projected-away var.
    "SELECT ?x WHERE { { SELECT ?x WHERE { ?x <http://ex/knows> ?y } } FILTER(?y = <http://ex/b>) }",
    // Predicate-position Eq -> SameTerm reduction exercised end-to-end.
    "SELECT ?s WHERE { ?s ?p ?o FILTER(?p = <http://ex/name>) }",
];

/// The full pipeline (nothing disabled) must equal the pipeline with all
/// four Phase-2 passes disabled at once — every query in the battery.
#[test]
fn full_pipeline_matches_all_passes_disabled() {
    let horn = fixture();
    let all_disabled: HashSet<PassId> = PHASE2_PASSES.iter().copied().collect();
    for q in QUERIES {
        let full = run_with(&horn, q, HashSet::new());
        let disabled = run_with(&horn, q, all_disabled.clone());
        assert_eq!(
            full, disabled,
            "full pipeline vs all-Phase-2-disabled diverged for:\n{q}"
        );
    }
}

/// Disabling any ONE Phase-2 pass must not change the result versus the
/// baseline (nothing disabled) — proves each pass is individually
/// result-invariant and gives a future regression a one-`PassId` bisection.
#[test]
fn each_pass_is_individually_result_invariant() {
    let horn = fixture();
    for q in QUERIES {
        let baseline = run_with(&horn, q, HashSet::new());
        for pass in PHASE2_PASSES {
            let disabled: HashSet<PassId> = HashSet::from([pass]);
            let with_pass_off = run_with(&horn, q, disabled);
            assert_eq!(
                baseline, with_pass_off,
                "disabling {pass:?} changed the result for:\n{q}"
            );
        }
    }
}
