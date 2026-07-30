//! Named-graph query semantics (SPEC-28 S3, PLAN-28-03 Task 3): ground
//! `GRAPH <g>`, `FROM`/`FROM NAMED` dataset construction, and the
//! `union`/`strict` default-graph mode — over both Stage-1 backends.
//!
//! Fixture (shared by every case):
//!
//! | graph | triples |
//! |---|---|
//! | default | t1 = `<a> <p> <o1>` |
//! | `<g1>` | t2 = `<b> <p> <o2>` |
//! | `<g2>` | t3 = `<c> <p> <o3>`, t2 (the *same* triple as in `<g1>`) |
//!
//! t2 living in two graphs is the point: a union default graph is a
//! **set** union, so t2 must come back once, not twice.

use horndb_sparql::api::{execute_query_with, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::{DefaultGraphMode, SparqlConfig};

const G1: &str = "http://ex/g1";
const G2: &str = "http://ex/g2";
/// A HornDB-internal graph (SPEC-27 F6 / SPEC-29 D4 namespace) — never part
/// of the no-dataset default graph, addressable only by explicit name.
const RESERVED: &str = "https://horndb.io/graph/x";

/// Insert quads into a backend. `None` is the default graph.
///
/// The write trait (`exec::Store`) is still triple-shaped and default-graph
/// only — the named-graph write path is SPEC-28 phase 4 (#267) — so these
/// tests plant quads through each backend's storage seam directly.
trait QuadSeed {
    fn seed_quad(&mut self, graph: Option<&str>, s: &str, p: &str, o: &str);
}

impl QuadSeed for MemStore {
    fn seed_quad(&mut self, graph: Option<&str>, s: &str, p: &str, o: &str) {
        self.insert_quad(graph, (s.to_owned(), p.to_owned(), o.to_owned()));
    }
}

impl QuadSeed for HornBackend {
    fn seed_quad(&mut self, graph: Option<&str>, s: &str, p: &str, o: &str) {
        let iri = |v: &str| oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v));
        match graph {
            None => {
                self.insert_oxrdf(&iri(s), &iri(p), &iri(o)).unwrap();
            }
            Some(g) => {
                self.insert_oxrdf_in_named_graph(&iri(g), &iri(s), &iri(p), &iri(o))
                    .unwrap();
            }
        }
    }
}

fn fixture<B: QuadSeed + Default>() -> B {
    let mut b = B::default();
    b.seed_quad(None, "http://ex/a", "http://ex/p", "http://ex/o1"); // t1
    b.seed_quad(Some(G1), "http://ex/b", "http://ex/p", "http://ex/o2"); // t2
    b.seed_quad(Some(G2), "http://ex/c", "http://ex/p", "http://ex/o3"); // t3
    b.seed_quad(Some(G2), "http://ex/b", "http://ex/p", "http://ex/o2"); // t2 again
    b
}

/// Run `q` and return the `?s` bindings, sorted, **with duplicates kept**
/// (dedup bugs must be visible).
fn subjects<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
    mode: DefaultGraphMode,
) -> Vec<String> {
    let cfg = SparqlConfig {
        default_graph: mode,
        ..SparqlConfig::default()
    };
    let QueryAnswer::Solutions { rows, .. } = execute_query_with(q, store, &cfg).unwrap() else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| match r.get("s") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI-bound ?s, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// `subjects` under the default (`union`) mode.
fn union_subjects<E: horndb_sparql::exec::Executor + ?Sized>(store: &E, q: &str) -> Vec<String> {
    subjects(store, q, DefaultGraphMode::Union)
}

const ALL: &str = "SELECT ?s WHERE { ?s ?p ?o }";

// --- ground GRAPH -----------------------------------------------------

fn ground_graph_scopes_to_one_graph<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_subjects(
            &b,
            &format!("SELECT ?s WHERE {{ GRAPH <{G1}> {{ ?s ?p ?o }} }}")
        ),
        vec!["http://ex/b"],
        "GRAPH <g1> must see exactly g1's one triple"
    );
}

fn unknown_graph_yields_zero_rows<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert!(
        union_subjects(
            &b,
            "SELECT ?s WHERE { GRAPH <http://ex/nope> { ?s ?p ?o } }"
        )
        .is_empty(),
        "an unknown graph IRI is zero rows, never an error"
    );
}

// --- the no-dataset default graph -------------------------------------

fn union_mode_unqualified_sees_all_non_reserved_deduped<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    assert_eq!(
        subjects(&b, ALL, DefaultGraphMode::Union),
        vec!["http://ex/a", "http://ex/b", "http://ex/c"],
        "union default graph is a SET union — t2 (in g1 and g2) appears once"
    );
}

fn strict_mode_unqualified_sees_default_only<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    assert_eq!(
        subjects(&b, ALL, DefaultGraphMode::Strict),
        vec!["http://ex/a"],
        "strict mode sees only the default-graph sentinel"
    );
}

fn reserved_graph_excluded_from_union<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let mut b: B = fixture();
    b.seed_quad(Some(RESERVED), "http://ex/r", "http://ex/p", "http://ex/o4");
    assert_eq!(
        subjects(&b, ALL, DefaultGraphMode::Union),
        vec!["http://ex/a", "http://ex/b", "http://ex/c"],
        "a reserved graph is never part of the no-dataset default graph"
    );
    assert_eq!(
        union_subjects(
            &b,
            &format!("SELECT ?s WHERE {{ GRAPH <{RESERVED}> {{ ?s ?p ?o }} }}")
        ),
        vec!["http://ex/r"],
        "naming a reserved graph explicitly is the opt-in"
    );
}

// --- FROM / FROM NAMED ------------------------------------------------

fn from_builds_union<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_subjects(
            &b,
            &format!("SELECT ?s FROM <{G1}> FROM <{G2}> WHERE {{ ?s ?p ?o }}")
        ),
        vec!["http://ex/b", "http://ex/c"],
        "FROM builds the default graph from exactly those graphs, deduped"
    );
}

fn from_named_only_empty_default_graph<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert!(
        union_subjects(
            &b,
            &format!("SELECT ?s FROM NAMED <{G1}> WHERE {{ ?s ?p ?o }}")
        )
        .is_empty(),
        "FROM NAMED without FROM leaves an EMPTY default graph (SPARQL 1.1 §13.2, D4)"
    );
}

fn from_named_restricts_ground_graph<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_subjects(
            &b,
            &format!("SELECT ?s FROM NAMED <{G1}> WHERE {{ GRAPH <{G1}> {{ ?s ?p ?o }} }}")
        ),
        vec!["http://ex/b"],
        "a named graph in the FROM NAMED set is addressable"
    );
    assert!(
        union_subjects(
            &b,
            &format!("SELECT ?s FROM NAMED <{G1}> WHERE {{ GRAPH <{G2}> {{ ?s ?p ?o }} }}")
        )
        .is_empty(),
        "a graph outside the FROM NAMED set yields zero rows"
    );
}

// --- pushdowns must not widen the scope -------------------------------

/// SPEC-28 S3's silent-wrong-answer clause: a `COUNT` pushed down to a
/// count leaf must count **within the scope**, never over the whole store.
/// `COUNT(*)` over a bare BGP is exactly the shape `plan::pushdown` lowers
/// to `CountScan`, so this is the pushdown path, not the scan fallback.
fn count_pushdown_respects_the_graph_scope<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    let count = |q: &str| -> String {
        let QueryAnswer::Solutions { rows, .. } =
            execute_query_with(q, &b, &SparqlConfig::default()).unwrap()
        else {
            panic!("expected solutions");
        };
        match rows[0].get("c") {
            Some(horndb_sparql::algebra::Term::Literal(l)) => {
                l.split('"').nth(1).unwrap_or_default().to_owned()
            }
            other => panic!("expected a count literal, got {other:?}"),
        }
    };
    // Whole (union) default graph: t1, t2, t3 — t2 deduped across g1/g2.
    assert_eq!(count("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }"), "3");
    // g1 holds exactly t2.
    assert_eq!(
        count(&format!(
            "SELECT (COUNT(*) AS ?c) WHERE {{ GRAPH <{G1}> {{ ?s ?p ?o }} }}"
        )),
        "1"
    );
    // An unknown graph counts nothing — never the whole store.
    assert_eq!(
        count("SELECT (COUNT(*) AS ?c) WHERE { GRAPH <http://ex/nope> { ?s ?p ?o } }"),
        "0"
    );
}

/// Two BGPs under *different* `GRAPH` wrappers must not be merged into one
/// flat scan by `CoalesceBgp` — one flat pattern set reads one graph, so a
/// merge would answer both patterns from whichever scope survived. `<a>`
/// lives only in the default graph and `<b>` only in `<g1>`, so a join
/// across the two scopes has a solution iff the scopes stayed apart.
fn bgps_under_different_graphs_do_not_coalesce<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    let QueryAnswer::Solutions { rows, .. } = execute_query_with(
        &format!(
            "SELECT ?x ?y WHERE {{ GRAPH <{G1}> {{ ?x <http://ex/p> <http://ex/o2> }} \
             GRAPH <{G2}> {{ ?y <http://ex/p> <http://ex/o3> }} }}"
        ),
        &b,
        &SparqlConfig::default(),
    )
    .unwrap() else {
        panic!("expected solutions");
    };
    assert_eq!(
        rows.len(),
        1,
        "one row joining g1's t2 with g2's t3: {rows:?}"
    );
}

// --- GRAPH ?g is Task 4: it must ERROR here, never answer wrongly ------

fn graph_var_errors_until_task_4<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    let err = execute_query_with(
        "SELECT ?s ?g WHERE { GRAPH ?g { ?s ?p ?o } }",
        &b,
        &SparqlConfig::default(),
    )
    .expect_err("GRAPH ?g must refuse, not approximate");
    let msg = err.to_string();
    assert!(
        msg.contains("GRAPH ?g"),
        "error must name the construct: {msg}"
    );
}

/// Instantiate every case above for both backends.
macro_rules! both_backends {
    ($($name:ident),+ $(,)?) => {
        $(
            mod $name {
                #[test]
                fn horn() {
                    super::$name::<horndb_sparql::exec::horn::HornBackend>();
                }
                #[test]
                fn mem() {
                    super::$name::<horndb_sparql::exec::mem::MemStore>();
                }
            }
        )+
    };
}

both_backends!(
    ground_graph_scopes_to_one_graph,
    unknown_graph_yields_zero_rows,
    union_mode_unqualified_sees_all_non_reserved_deduped,
    strict_mode_unqualified_sees_default_only,
    reserved_graph_excluded_from_union,
    from_builds_union,
    from_named_only_empty_default_graph,
    from_named_restricts_ground_graph,
    count_pushdown_respects_the_graph_scope,
    bgps_under_different_graphs_do_not_coalesce,
    graph_var_errors_until_task_4,
);
