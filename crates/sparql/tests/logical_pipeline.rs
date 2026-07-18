//! SPEC-23 Phase-1 gate: the logical pipeline reproduces today's physical
//! plans (no behavior change), coalescing a flat BGP is result-invariant, and
//! passes are individually disable-able.

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

#[test]
fn pipeline_reproduces_todays_physical_plans() {
    for q in GOLDEN_QUERIES {
        let alg = algebra_of(q);
        assert_eq!(
            planner::plan(&alg).expect("plan"),
            reference_plan(&alg),
            "logical pipeline changed the physical plan for:\n{q}"
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
    let coalesced = lower_physical(&run_passes(
        lower_algebra(&join_alg),
        &standard_passes(),
        &PlanCtx::default(),
    ));
    let nested = lower_physical(&run_passes(
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
