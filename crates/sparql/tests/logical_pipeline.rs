//! SPEC-23 Phase-1 gate: the logical *lowering* reproduces today's physical
//! plans (with the shape-changing Phase-2 rewrite passes disabled),
//! coalescing a flat BGP is result-invariant, and passes are individually
//! disable-able.

use horndb_sparql::algebra::translate::translate_query_with;
use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::{Bindings, Store};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::lower::{lower_algebra, lower_physical};
use horndb_sparql::plan::pass::{run_passes, standard_passes, PassId, PlanCtx};
use horndb_sparql::plan::{planner, PhysicalPlan};
use horndb_sparql::SparqlConfig;
use std::collections::HashSet;

fn algebra_of(q: &str) -> Algebra {
    let inner = match parse_query(q).expect("parse") {
        ParsedQuery::Select { inner } => inner,
        other => panic!("expected SELECT, got {other:?}"),
    };
    translate_query_with(&inner, &SparqlConfig::default()).expect("translate")
}

/// The pre-refactor reference: a straight 1:1 Algebra → PhysicalPlan lowering,
/// frozen here so the golden comparison is self-contained.
fn reference_plan(alg: &Algebra) -> PhysicalPlan {
    match alg {
        Algebra::Bgp { patterns } => PhysicalPlan::BgpScan {
            patterns: patterns.clone(),
        },
        Algebra::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
        },
        Algebra::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
            expr: expr.clone(),
        },
        Algebra::Filter { expr, inner } => PhysicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
        },
        Algebra::Project { vars, inner } => PhysicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => PhysicalPlan::Slice {
            inner: Box::new(reference_plan(inner)),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(reference_plan(inner)),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(reference_plan(inner)),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => PhysicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => PhysicalPlan::Group {
            inner: Box::new(reference_plan(inner)),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PhysicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(reference_plan(edge)),
            reflexive: *reflexive,
        },
    }
}

/// Representative query battery spanning every algebra operator.
const GOLDEN_QUERIES: &[&str] = &[
    "SELECT * WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s ?p ?o }",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age > \"20\") }",
    "SELECT ?n (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
    "SELECT ?s WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
    "SELECT ?x WHERE { { ?x <http://ex/name> ?n } UNION { ?x <http://ex/age> ?age } }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age BIND(?age AS ?b) }",
    "SELECT DISTINCT ?n WHERE { ?s <http://ex/name> ?n } ORDER BY ?n LIMIT 2 OFFSET 1",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . { SELECT ?s ?n WHERE { ?s <http://ex/name> ?n } } }",
    "SELECT ?x ?y WHERE { ?x <http://ex/sco>+ ?y }",
];

/// The Phase-1 golden gate: lowering Algebra → LogicalPlan → PhysicalPlan
/// is 1:1 with the frozen [`reference_plan`]. The Phase-2 rewrite passes
/// (Normalize, FilterPullup, FilterPushdown, ProjectionPushdown)
/// deliberately change plan *shapes*, so they are disabled here — this test
/// pins lowering fidelity, not the rewrites. The rewrites' own guarantee
/// (result invariance) will be covered by the rewrite-invariance battery
/// (tests/rewrite_invariance.rs) and the per-pass unit tests instead.
#[test]
fn pipeline_reproduces_todays_physical_plans() {
    let ctx = horndb_sparql::plan::pass::PlanCtx {
        disabled_passes: HashSet::from([
            PassId::Normalize,
            PassId::FilterPullup,
            PassId::FilterPushdown,
            PassId::ProjectionPushdown,
        ]),
    };
    for q in GOLDEN_QUERIES {
        let alg = algebra_of(q);
        assert_eq!(
            planner::plan_with_ctx(&alg, &ctx).expect("plan"),
            reference_plan(&alg),
            "logical lowering changed the physical plan for:\n{q}"
        );
    }
}

fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
    TriplePattern {
        subject: Term::Var(Var::new(s)),
        predicate: Term::Iri(p.to_owned()),
        object: Term::Var(Var::new(o)),
    }
}

/// Hand-built `Algebra::Join { Bgp, Bgp }` (spargebra never emits this, so it
/// is the only way to exercise coalescing end-to-end). The coalesced flat
/// `BgpScan{[p1,p2]}` and the nested `Join(BgpScan{[p1]},BgpScan{[p2]})` must
/// produce identical result multisets — the WCOJ executor runs the whole
/// pattern set as one natural join, same as the hash join over shared vars.
#[test]
fn coalesced_bgp_is_result_equivalent_to_nested_join() {
    let mut horn = HornBackend::new();
    let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
    horn.insert_triple(iri("a"), iri("p"), iri("b"));
    horn.insert_triple(iri("b"), iri("q"), iri("c"));
    horn.insert_triple(iri("a"), iri("p"), iri("x"));
    horn.insert_triple(iri("x"), iri("q"), iri("y"));

    let join_alg = Algebra::Join {
        left: Box::new(Algebra::Bgp {
            patterns: vec![pat("s", "http://ex/p", "o")],
        }),
        right: Box::new(Algebra::Bgp {
            patterns: vec![pat("o", "http://ex/q", "z")],
        }),
    };

    // Coalesced (CoalesceBgp on) vs nested (CoalesceBgp disabled).
    let coalesced = lower_physical(run_passes(
        lower_algebra(&join_alg),
        &standard_passes(),
        &PlanCtx::default(),
    ));
    let nested = lower_physical(run_passes(
        lower_algebra(&join_alg),
        &standard_passes(),
        &PlanCtx {
            disabled_passes: HashSet::from([PassId::CoalesceBgp]),
        },
    ));
    assert!(matches!(coalesced, PhysicalPlan::BgpScan { .. }));
    assert!(matches!(nested, PhysicalPlan::Join { .. }));

    let canon = |mut rows: Vec<Bindings>| -> Vec<String> {
        let mut v: Vec<String> = rows
            .drain(..)
            .map(|b| {
                b.vars()
                    .map(|(k, t)| format!("{k}={t:?}"))
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            })
            .collect();
        v.sort();
        v
    };
    let a: Vec<Bindings> = Runtime::new(&horn).run(&coalesced).unwrap().collect();
    let b: Vec<Bindings> = Runtime::new(&horn).run(&nested).unwrap().collect();
    assert_eq!(canon(a), canon(b), "coalescing changed the result set");
}

mod pragma {
    use horndb_sparql::api::{execute_query, QueryAnswer};
    use horndb_sparql::exec::mem::MemStore;
    use horndb_sparql::parser::strip_plan_pragmas;
    use horndb_sparql::plan::pass::PassId;

    #[test]
    fn strips_one_disable_pass_pragma() {
        let (rest, disabled) =
            strip_plan_pragmas("PRAGMA disable-pass=coalesce-bgp SELECT * WHERE { ?s ?p ?o }")
                .expect("pragma parses");
        assert!(rest.trim_start().starts_with("SELECT"));
        assert!(disabled.contains(&PassId::CoalesceBgp));
    }

    #[test]
    fn strips_multiple_pragmas() {
        let (rest, disabled) = strip_plan_pragmas(
            "PRAGMA disable-pass=coalesce-bgp PRAGMA disable-pass=join-planning ASK { ?s ?p ?o }",
        )
        .expect("pragmas parse");
        assert!(rest.trim_start().starts_with("ASK"));
        assert!(disabled.contains(&PassId::CoalesceBgp));
        assert!(disabled.contains(&PassId::JoinPlanning));
    }

    #[test]
    fn no_pragma_is_identity() {
        let (rest, disabled) =
            strip_plan_pragmas("SELECT * WHERE { ?s ?p ?o }").expect("no pragma");
        assert_eq!(rest, "SELECT * WHERE { ?s ?p ?o }");
        assert!(disabled.is_empty());
    }

    #[test]
    fn unknown_pass_id_is_an_error() {
        assert!(
            strip_plan_pragmas("PRAGMA disable-pass=nope SELECT * WHERE { ?s ?p ?o }").is_err()
        );
    }

    /// End-to-end: a pragma-carrying query still runs and returns results
    /// (the pragma is consumed, not passed to spargebra).
    #[test]
    fn pragma_query_executes_and_returns_results() {
        let mut s = MemStore::default();
        s.insert(("a".into(), "p".into(), "b".into()));
        let ans = execute_query(
            "PRAGMA disable-pass=coalesce-bgp SELECT * WHERE { ?s ?p ?o }",
            &s,
        )
        .expect("pragma query runs");
        match ans {
            QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 1),
            other => panic!("expected Solutions, got {other:?}"),
        }
    }
}

/// The one real-query shape where `CoalesceBgp` fires today: HornDB's
/// Stage-1 `GRAPH` lowering (merged-graph semantics — `GRAPH <g> { P }`
/// lowers to `P`) produces `Algebra::Join(Bgp, Bgp)` when a query mixes
/// top-level triples with a `GRAPH` block. The pipeline coalesces that into
/// one flat `BgpScan` (SPEC-23 §5.1: widest pattern set for the WCOJ
/// planner). This test pins the coalesced shape AND proves the results are
/// unchanged versus the pass disabled — including the disjoint-variable
/// (cross-product) case, where the flat scan must still equal the nested
/// join.
#[test]
fn graph_adjacent_bgps_coalesce_and_stay_result_equivalent() {
    let mut horn = HornBackend::new();
    let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
    horn.insert_triple(iri("a"), iri("p"), iri("b"));
    horn.insert_triple(iri("c"), iri("p"), iri("d"));
    horn.insert_triple(iri("x"), iri("q"), iri("y"));
    horn.insert_triple(iri("z"), iri("q"), iri("w"));

    // Disjoint variables across the two groups → a genuine cross product.
    let q = "SELECT * WHERE { ?s <http://ex/p> ?o . GRAPH <http://ex/g> { ?a <http://ex/q> ?b } }";
    let alg = algebra_of(q);

    let coalesced = planner::plan(&alg).expect("plan");
    let nested = planner::plan_with_ctx(
        &alg,
        &horndb_sparql::plan::pass::PlanCtx {
            disabled_passes: HashSet::from([PassId::CoalesceBgp]),
        },
    )
    .expect("plan");

    // Shape: coalesced plan carries one flat 2-pattern scan; disabled plan
    // keeps the nested Join the old lowering emitted.
    fn find_bgp_sizes(p: &PhysicalPlan, out: &mut Vec<usize>) -> bool {
        match p {
            PhysicalPlan::BgpScan { patterns } => {
                out.push(patterns.len());
                false
            }
            PhysicalPlan::Join { left, right } => {
                find_bgp_sizes(left, out);
                find_bgp_sizes(right, out);
                true
            }
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::Filter { inner, .. } => find_bgp_sizes(inner, out),
            _ => false,
        }
    }
    let mut sizes = Vec::new();
    find_bgp_sizes(&coalesced, &mut sizes);
    assert_eq!(
        sizes,
        vec![2],
        "coalesced plan must hold one flat 2-pattern scan"
    );
    let mut nested_sizes = Vec::new();
    find_bgp_sizes(&nested, &mut nested_sizes);
    assert_eq!(nested_sizes, vec![1, 1], "disabled plan keeps two scans");

    // Results: identical multisets (4-row cross product on this data).
    let canon = |rows: Vec<Bindings>| -> Vec<String> {
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
    };
    let a: Vec<Bindings> = Runtime::new(&horn).run(&coalesced).unwrap().collect();
    let b: Vec<Bindings> = Runtime::new(&horn).run(&nested).unwrap().collect();
    assert_eq!(a.len(), 4, "2x2 cross product expected");
    assert_eq!(
        canon(a),
        canon(b),
        "coalescing changed GRAPH-adjacent results"
    );
}

mod plan_select_pragmas {
    use horndb_sparql::api::plan_select;
    use horndb_sparql::SparqlConfig;

    /// The HTTP /query handler routes every request through plan_select
    /// first — a pragma-carrying SELECT must plan (not 400 as a spargebra
    /// parse error), and a pragma-carrying non-SELECT must return Ok(None)
    /// so the handler falls back to the materialized path.
    #[test]
    fn pragma_select_plans_on_the_streaming_path() {
        let out = plan_select(
            "PRAGMA disable-pass=coalesce-bgp SELECT * WHERE { ?s ?p ?o }",
            &SparqlConfig::default(),
        )
        .expect("pragma SELECT must parse on the streaming path");
        assert!(out.is_some(), "SELECT must yield a streaming plan");
    }

    #[test]
    fn pragma_ask_falls_back_to_materialized() {
        let out = plan_select(
            "PRAGMA disable-pass=coalesce-bgp ASK { ?s ?p ?o }",
            &SparqlConfig::default(),
        )
        .expect("pragma ASK must not be a parse error");
        assert!(out.is_none(), "non-SELECT falls back (Ok(None))");
    }
}
