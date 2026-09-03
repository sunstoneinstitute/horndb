//! SPEC-29 P1 acceptance — declared reasoning views over named graphs.
//!
//! Every test drives derivation synchronously through
//! `ViewManager::run_until_clean`, so nothing here depends on the background
//! worker's timing.

#![cfg(feature = "reasoner")]

use horndb_config::{Reasoning, ViewSelect, Views};
use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{AlgebraQuad, Store};
use horndb_sparql::reasoning::{ViewManager, ViewSource, SPINE_CLOSURE_GRAPH, VIEWS_GRAPH};

const VOCAB: &str = "https://ex.org/vocab/ont";
const G1: &str = "https://ex.org/data/g1";
const G2: &str = "https://ex.org/data/g2";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";

fn cfg(enabled: bool) -> Reasoning {
    Reasoning {
        enabled,
        spine: vec!["https://ex.org/vocab/".to_string()],
        views: Views::default(),
        default_dataset_includes_inferred: false,
        ..Reasoning::default()
    }
}

fn seed(store: &mut HornBackend, g: Option<&str>, s: &str, p: &str, o: &str) {
    let q: AlgebraQuad = (
        g.map(str::to_owned),
        Term::Iri(s.into()),
        Term::Iri(p.into()),
        Term::Iri(o.into()),
    );
    store.apply_quads(Vec::new(), vec![q]).unwrap();
}

fn drop_quad(store: &mut HornBackend, g: Option<&str>, s: &str, p: &str, o: &str) {
    let q: AlgebraQuad = (
        g.map(str::to_owned),
        Term::Iri(s.into()),
        Term::Iri(p.into()),
        Term::Iri(o.into()),
    );
    store.apply_quads(vec![q], Vec::new()).unwrap();
}

/// Triples in one named graph, read through the query path (`GRAPH <g>`),
/// which is how a client would see them.
fn graph_triples(store: &HornBackend, g: &str) -> Vec<(String, String, String)> {
    let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}");
    let QueryAnswer::Solutions { rows, vars } = execute_query(&q, store).unwrap() else {
        panic!("expected solutions");
    };
    assert_eq!(vars, vec!["s", "p", "o"]);
    let mut out: Vec<_> = rows
        .into_iter()
        .map(|r| {
            let f = |v: &str| match r.get(v) {
                Some(Term::Iri(s)) => s.clone(),
                other => format!("{other:?}"),
            };
            (f("s"), f("p"), f("o"))
        })
        .collect();
    out.sort();
    out
}

fn inferred_of(g: &str) -> String {
    ViewSource::Named(g.to_string()).inferred_graph()
}

/// A vocabulary the spine holds and two data graphs that instantiate it.
fn seeded_store() -> HornBackend {
    let mut store = HornBackend::new();
    seed(
        &mut store,
        Some(VOCAB),
        "https://ex.org/C",
        SUB_CLASS_OF,
        "https://ex.org/D",
    );
    seed(
        &mut store,
        Some(G1),
        "https://ex.org/a",
        TYPE,
        "https://ex.org/C",
    );
    seed(
        &mut store,
        Some(G2),
        "https://ex.org/b",
        TYPE,
        "https://ex.org/C",
    );
    store
}

/// SPEC-29 D1/D2/D3/D4: a view derives spine × its own data, and lands under
/// the reserved namespace rather than in the source graph.
#[test]
fn view_derives_spine_x_data() {
    let mut store = seeded_store();
    let mut mgr = ViewManager::new(&cfg(true));
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 2);

    assert!(graph_triples(&store, &inferred_of(G1)).contains(&(
        "https://ex.org/a".into(),
        TYPE.into(),
        "https://ex.org/D".into()
    )));
    assert!(graph_triples(&store, &inferred_of(G2)).contains(&(
        "https://ex.org/b".into(),
        TYPE.into(),
        "https://ex.org/D".into()
    )));
}

/// SPEC-29 D2's isolation guarantee: `g1`'s data never entails anything in
/// `g2`'s view. The probe is a subclass axiom asserted *in a data graph* —
/// under the pre-SPEC-29 "reason over everything loaded" behaviour it would
/// fire for the other graph's individual too.
#[test]
fn isolation_two_graphs_no_cross_entailment() {
    let mut store = seeded_store();
    // Only g1 says C ⊑ E.
    seed(
        &mut store,
        Some(G1),
        "https://ex.org/C",
        SUB_CLASS_OF,
        "https://ex.org/E",
    );
    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();

    let e = |s: &str| {
        (
            s.to_string(),
            TYPE.to_string(),
            "https://ex.org/E".to_string(),
        )
    };
    assert!(graph_triples(&store, &inferred_of(G1)).contains(&e("https://ex.org/a")));
    let g2 = graph_triples(&store, &inferred_of(G2));
    assert!(
        !g2.iter().any(|t| t.2 == "https://ex.org/E"),
        "g1's axiom leaked into g2's view: {g2:?}"
    );
}

/// SPEC-29 D7 + acceptance 6: a write to one data graph re-derives exactly one
/// view, and a no-op re-run derives none.
#[test]
fn single_graph_update_derives_one_view() {
    let mut store = seeded_store();
    let mut mgr = ViewManager::new(&cfg(true));
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 2);
    assert_eq!(
        mgr.run_until_clean(&mut store).unwrap(),
        0,
        "an unchanged store must derive nothing"
    );

    seed(
        &mut store,
        Some(G1),
        "https://ex.org/a2",
        TYPE,
        "https://ex.org/C",
    );
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 1);
    assert!(graph_triples(&store, &inferred_of(G1)).contains(&(
        "https://ex.org/a2".into(),
        TYPE.into(),
        "https://ex.org/D".into()
    )));
}

/// SPEC-29 D5: reasoning never writes a source graph, so reading one back
/// returns exactly the quads that were written to it — including the default
/// graph, which also gets a view.
#[test]
fn source_graph_read_returns_exactly_what_was_written() {
    let mut store = seeded_store();
    seed(
        &mut store,
        None,
        "https://ex.org/d",
        TYPE,
        "https://ex.org/C",
    );
    let before_g1 = graph_triples(&store, G1);
    let before_vocab = graph_triples(&store, VOCAB);

    let mut mgr = ViewManager::new(&cfg(true));
    // Three views: g1, g2, and the (non-empty) default graph.
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 3);

    assert_eq!(graph_triples(&store, G1), before_g1);
    assert_eq!(graph_triples(&store, VOCAB), before_vocab);
    assert!(store
        .scan_graph_lexical(None)
        .unwrap()
        .iter()
        .all(|t| t.2 == "https://ex.org/C"));
    assert!(
        graph_triples(&store, &ViewSource::Default.inferred_graph()).contains(&(
            "https://ex.org/d".into(),
            TYPE.into(),
            "https://ex.org/D".into()
        ))
    );
}

/// SPEC-29 D3: a spine edit marks every view stale and they all converge —
/// including across a simulated restart, where the manager is thrown away and
/// rebuilt with no memory of what it had derived.
#[test]
fn spine_edit_marks_all_dirty_and_converges() {
    let mut store = seeded_store();
    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();
    let v0 = mgr.catalog().spine_version();

    seed(
        &mut store,
        Some(VOCAB),
        "https://ex.org/D",
        SUB_CLASS_OF,
        "https://ex.org/F",
    );
    assert_eq!(
        mgr.run_until_clean(&mut store).unwrap(),
        2,
        "both views stale"
    );
    assert_eq!(mgr.catalog().spine_version(), v0 + 1);

    let f = |s: &str| {
        (
            s.to_string(),
            TYPE.to_string(),
            "https://ex.org/F".to_string(),
        )
    };
    let after = graph_triples(&store, &inferred_of(G1));
    assert!(after.contains(&f("https://ex.org/a")));

    // Restart: a fresh manager re-derives from scratch and lands on exactly
    // the same contents (the derivation is an idempotent diff).
    let mut fresh = ViewManager::new(&cfg(true));
    fresh.run_until_clean(&mut store).unwrap();
    assert_eq!(graph_triples(&store, &inferred_of(G1)), after);

    // Retracting the axiom retracts the inference: the diff deletes, it does
    // not only add.
    drop_quad(
        &mut store,
        Some(VOCAB),
        "https://ex.org/D",
        SUB_CLASS_OF,
        "https://ex.org/F",
    );
    fresh.run_until_clean(&mut store).unwrap();
    assert!(!graph_triples(&store, &inferred_of(G1)).contains(&f("https://ex.org/a")));
}

/// SPEC-29 D3 condition 2: an inconsistent view is flagged, not fatal — the
/// other view keeps deriving and the store keeps answering.
#[test]
fn inconsistent_view_flagged_not_fatal() {
    let mut store = seeded_store();
    seed(
        &mut store,
        Some(VOCAB),
        "https://ex.org/C",
        DISJOINT_WITH,
        "https://ex.org/H",
    );
    seed(
        &mut store,
        Some(G1),
        "https://ex.org/a",
        TYPE,
        "https://ex.org/H",
    );

    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();

    let views = mgr.catalog().views();
    assert!(
        !views[&ViewSource::Named(G1.into())].consistent,
        "g1 contradicts the disjointness axiom"
    );
    assert!(
        views[&ViewSource::Named(G2.into())].consistent,
        "g2 must be unaffected by g1's contradiction"
    );
    // The store still answers, and g2's view still derived.
    assert!(graph_triples(&store, &inferred_of(G2)).contains(&(
        "https://ex.org/b".into(),
        TYPE.into(),
        "https://ex.org/D".into()
    )));
}

/// The `reasoning.enabled = false` no-op path: no engine runs and not one
/// reserved graph appears.
#[test]
fn disabled_means_no_reserved_graphs() {
    let mut store = seeded_store();
    let before = store.graphs();
    let mut mgr = ViewManager::new(&cfg(false));
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 0);
    assert_eq!(store.graphs(), before);
    assert!(mgr.catalog().views().is_empty());
}

/// SPEC-29 D3: the spine's own closure lives once in the shared spine-closure
/// graph, and is not replicated into any view's inferred graph.
#[test]
fn spine_closure_is_shared_not_replicated() {
    let mut store = seeded_store();
    seed(
        &mut store,
        Some(VOCAB),
        "https://ex.org/D",
        SUB_CLASS_OF,
        "https://ex.org/F",
    );
    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();

    // C ⊑ F is derived from the spine alone, so it belongs to the shared
    // graph and to no view.
    let transitive = (
        "https://ex.org/C".to_string(),
        SUB_CLASS_OF.to_string(),
        "https://ex.org/F".to_string(),
    );
    assert!(graph_triples(&store, SPINE_CLOSURE_GRAPH).contains(&transitive));
    for g in [G1, G2] {
        assert!(
            !graph_triples(&store, &inferred_of(g)).contains(&transitive),
            "{g}'s view replicated a spine-only inference"
        );
    }
}

/// SPEC-29 D4: the catalog is queryable, one node per view, and its staleness
/// flags track reality.
#[test]
fn catalog_quads_readable() {
    let mut store = seeded_store();
    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();

    let q = format!(
        "SELECT ?v WHERE {{ GRAPH <{VIEWS_GRAPH}> \
         {{ ?v <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
            <https://horndb.io/ns/reasoning#View> }} }}"
    );
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, &store).unwrap() else {
        panic!("expected solutions");
    };
    let mut got: Vec<String> = rows
        .into_iter()
        .map(|r| match r.get("v") {
            Some(Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI, got {other:?}"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec![inferred_of(G1), inferred_of(G2)]);

    let stale = format!(
        "SELECT ?v WHERE {{ GRAPH <{VIEWS_GRAPH}> \
         {{ ?v <https://horndb.io/ns/reasoning#stale> \
            \"true\"^^<http://www.w3.org/2001/XMLSchema#boolean> }} }}"
    );
    let QueryAnswer::Solutions { rows, .. } = execute_query(&stale, &store).unwrap() else {
        panic!("expected solutions");
    };
    assert!(rows.is_empty(), "nothing is stale right after a clean pass");
}

/// SPEC-29 D6: inferred graphs stay out of the no-dataset default union and
/// out of `GRAPH ?g` unless the flag opts them in — and the catalog graph
/// stays hidden either way.
#[test]
fn default_dataset_includes_inferred_flag() {
    fn enumerated(store: &HornBackend) -> Vec<String> {
        let QueryAnswer::Solutions { rows, .. } =
            execute_query("SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }", store).unwrap()
        else {
            panic!("expected solutions");
        };
        let mut out: Vec<String> = rows
            .into_iter()
            .map(|r| match r.get("g") {
                Some(Term::Iri(s)) => s.clone(),
                other => panic!("expected an IRI, got {other:?}"),
            })
            .collect();
        out.sort();
        out
    }

    let mut store = seeded_store();
    let mut mgr = ViewManager::new(&cfg(true));
    mgr.run_until_clean(&mut store).unwrap();
    assert_eq!(
        enumerated(&store),
        vec![G1.to_string(), G2.to_string(), VOCAB.to_string()]
    );

    let mut on = cfg(true);
    on.default_dataset_includes_inferred = true;
    let mut mgr = ViewManager::new(&on);
    mgr.run_until_clean(&mut store).unwrap();
    let visible = enumerated(&store);
    assert!(visible.contains(&inferred_of(G1)));
    assert!(visible.contains(&SPINE_CLOSURE_GRAPH.to_string()));
    assert!(
        !visible.contains(&VIEWS_GRAPH.to_string()),
        "the catalog graph is not part of the data: {visible:?}"
    );
}

/// The degenerate case SPEC-29 must not regress: no named graphs at all, so
/// the single default-graph view reproduces the pre-SPEC-29 whole-store
/// materialization.
#[test]
fn degenerate_default_graph_view_matches_legacy() {
    let mut store = HornBackend::new();
    seed(
        &mut store,
        None,
        "https://ex.org/C",
        SUB_CLASS_OF,
        "https://ex.org/D",
    );
    seed(
        &mut store,
        None,
        "https://ex.org/a",
        TYPE,
        "https://ex.org/C",
    );

    let mut mgr = ViewManager::new(&cfg(true));
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 1);

    let mut engine = horndb_owlrl::Engine::new();
    engine
        .load_base(store.scan_graph_lexical(None).unwrap())
        .unwrap();
    let mut excluded: std::collections::BTreeSet<_> = store
        .scan_graph_lexical(None)
        .unwrap()
        .into_iter()
        .collect();
    // The empty spine's own closure: the datatype axioms the engine injects
    // regardless of input. The view subtracts them (they belong to the shared
    // spine-closure graph, not to any view), so the legacy baseline must too.
    let mut empty = horndb_owlrl::Engine::new();
    empty.load_base(Vec::new()).unwrap();
    excluded.extend(empty.materialized_triples().unwrap());

    let mut legacy: Vec<_> = engine
        .materialized_triples()
        .unwrap()
        .into_iter()
        .filter(|t| !excluded.contains(t))
        .collect();
    legacy.sort();

    let mut got = store
        .scan_graph_lexical(Some(ViewSource::Default.inferred_graph()))
        .unwrap();
    got.sort();
    assert_eq!(got, legacy);
}

/// Selecting a subset with `views.select` leaves the unselected graph without
/// a view at all — nothing is derived for it.
#[test]
fn views_select_narrows_membership() {
    let mut store = seeded_store();
    let mut narrowed = cfg(true);
    narrowed.views.select = ViewSelect::Patterns(vec![G1.to_string()]);
    let mut mgr = ViewManager::new(&narrowed);
    assert_eq!(mgr.run_until_clean(&mut store).unwrap(), 1);
    assert_eq!(
        mgr.catalog().views().keys().cloned().collect::<Vec<_>>(),
        vec![ViewSource::Named(G1.into())]
    );
    assert!(graph_triples(&store, &inferred_of(G2)).is_empty());
}
