//! Named-graph query semantics (SPEC-28 S3): ground `GRAPH <g>`, variable
//! `GRAPH ?g` and its graph column, `FROM`/`FROM NAMED` dataset
//! construction, and the `union`/`strict` default-graph mode — over both
//! Stage-1 backends.
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

/// Run `q` and return its `(?g, ?s)` bindings, sorted, **with duplicates
/// kept** — one triple in two graphs must show up as two rows (SPEC-28 D6).
/// An unbound `?g` is a failure, not an empty string: `GRAPH ?g` binds it in
/// every row it emits.
fn graph_rows<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
    mode: DefaultGraphMode,
) -> Vec<(String, String)> {
    let cfg = SparqlConfig {
        default_graph: mode,
        ..SparqlConfig::default()
    };
    let QueryAnswer::Solutions { rows, .. } = execute_query_with(q, store, &cfg).unwrap() else {
        panic!("expected solutions");
    };
    let iri = |r: &horndb_sparql::exec::Bindings, v: &str| match r.get(v) {
        Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
        other => panic!("expected an IRI-bound ?{v}, got {other:?}"),
    };
    let mut out: Vec<(String, String)> = rows.iter().map(|r| (iri(r, "g"), iri(r, "s"))).collect();
    out.sort();
    out
}

/// `graph_rows` under the default (`union`) mode.
fn union_graph_rows<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
) -> Vec<(String, String)> {
    graph_rows(store, q, DefaultGraphMode::Union)
}

/// The fixture's `(?g, ?s)` rows for an unrestricted `GRAPH ?g`: g1 holds
/// t2, g2 holds t2 and t3. The default graph's t1 is absent (D3).
fn all_graph_rows() -> Vec<(String, String)> {
    vec![
        (G1.to_owned(), "http://ex/b".to_owned()),
        (G2.to_owned(), "http://ex/b".to_owned()),
        (G2.to_owned(), "http://ex/c".to_owned()),
    ]
}

const GRAPH_VAR_ALL: &str = "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o } }";

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

// --- GRAPH ?g: the graph column (SPEC-28 S3/D6) ------------------------

/// D3: `?g` ranges over the named graphs only — never the default graph —
/// and the `default_graph` mode does not touch that range.
fn graph_var_enumerates_named_graphs_only<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    for mode in [DefaultGraphMode::Union, DefaultGraphMode::Strict] {
        assert_eq!(
            graph_rows(&b, GRAPH_VAR_ALL, mode),
            all_graph_rows(),
            "GRAPH ?g enumerates g1 and g2 only, in {mode:?} mode"
        );
    }
}

/// A triple in two graphs is two solutions with different `?g` — the exact
/// opposite of the union default graph's set semantics.
fn graph_var_binds_per_row<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s <http://ex/p> <http://ex/o2> } }"
        ),
        vec![
            (G1.to_owned(), "http://ex/b".to_owned()),
            (G2.to_owned(), "http://ex/b".to_owned()),
        ],
        "t2 lives in g1 and g2, so it yields one row per graph"
    );
}

/// `FROM NAMED` is the named set `?g` ranges over.
fn graph_var_restricted_by_from_named<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_graph_rows(
            &b,
            &format!("SELECT ?g ?s FROM NAMED <{G1}> WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}")
        ),
        vec![(G1.to_owned(), "http://ex/b".to_owned())],
        "FROM NAMED <g1> leaves exactly one graph to enumerate"
    );
}

/// The `?g` column joins like any other: against a constant through a
/// `FILTER`, and against a value another pattern binds.
fn graph_var_join_with_ground_var<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let mut b: B = fixture();
    // A default-graph triple naming g1 — the join partner below.
    b.seed_quad(None, G1, "http://ex/says", "http://ex/hi");
    assert_eq!(
        union_graph_rows(
            &b,
            &format!(
                "SELECT ?g ?s WHERE {{ GRAPH ?g {{ ?s <http://ex/p> ?o }} FILTER(?g = <{G1}>) }}"
            )
        ),
        vec![(G1.to_owned(), "http://ex/b".to_owned())],
        "FILTER on the graph column restricts it like any bound variable"
    );
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { ?g <http://ex/says> <http://ex/hi> . \
             GRAPH ?g { ?s <http://ex/p> ?o } }"
        ),
        vec![(G1.to_owned(), "http://ex/b".to_owned())],
        "a shared ?g joins the graph column against another pattern's binding"
    );
}

/// `SELECT *` projects the graph variable, bound (it used to be scoped in
/// but never bindable).
fn select_star_projects_graph_var<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_graph_rows(&b, "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }"),
        all_graph_rows(),
        "SELECT * must carry a bound ?g"
    );
}

/// `GRAPH ?g { ?g … }` — the pattern binds the graph variable itself, so
/// the two must agree: a row survives only in the graph it names.
fn graph_var_bound_by_the_pattern_itself<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let mut b: B = fixture();
    b.seed_quad(Some(G1), G1, "http://ex/label", "http://ex/l"); // agrees
    b.seed_quad(Some(G2), G1, "http://ex/label", "http://ex/l"); // g2 holds a statement ABOUT g1
    let QueryAnswer::Solutions { rows, .. } = execute_query_with(
        "SELECT ?g WHERE { GRAPH ?g { ?g <http://ex/label> ?o } }",
        &b,
        &SparqlConfig::default(),
    )
    .unwrap() else {
        panic!("expected solutions");
    };
    let mut graphs: Vec<String> = rows
        .iter()
        .map(|r| match r.get("g") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI-bound ?g, got {other:?}"),
        })
        .collect();
    graphs.sort();
    assert_eq!(
        graphs,
        vec![G1.to_owned()],
        "g2's statement about g1 must not make ?g bind g1 in g2's scan"
    );
}

/// A `COUNT` pushed down under `GRAPH ?g` must count the per-graph rows —
/// the count seams have no per-graph form, so they decline and the scan
/// loop answers (SPEC-28 S3's silent-wrong-answer clause).
fn graph_var_count_counts_every_graph<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    let rows_of = |q: &str| -> Vec<(Option<String>, String)> {
        let QueryAnswer::Solutions { rows, .. } =
            execute_query_with(q, &b, &SparqlConfig::default()).unwrap()
        else {
            panic!("expected solutions");
        };
        let mut out: Vec<(Option<String>, String)> = rows
            .iter()
            .map(|r| {
                let g = match r.get("g") {
                    Some(horndb_sparql::algebra::Term::Iri(s)) => Some(s.clone()),
                    None => None,
                    other => panic!("expected an IRI-bound ?g, got {other:?}"),
                };
                let c = match r.get("c") {
                    Some(horndb_sparql::algebra::Term::Literal(l)) => {
                        l.split('"').nth(1).unwrap_or_default().to_owned()
                    }
                    other => panic!("expected a count literal, got {other:?}"),
                };
                (g, c)
            })
            .collect();
        out.sort();
        out
    };
    // t2 in g1, t2 in g2, t3 in g2 — three per-graph solutions, no dedup.
    assert_eq!(
        rows_of("SELECT (COUNT(*) AS ?c) WHERE { GRAPH ?g { ?s ?p ?o } }"),
        vec![(None, "3".to_owned())]
    );
    assert_eq!(
        rows_of("SELECT ?g (COUNT(*) AS ?c) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g"),
        vec![
            (Some(G1.to_owned()), "1".to_owned()),
            (Some(G2.to_owned()), "2".to_owned()),
        ]
    );
}

/// D6: the plan holds **one** scan node however many graphs `?g` ranges
/// over — the graph loop lives in the operator, not in the plan. (A union
/// of per-graph plans would show one `BgpScan` per graph here.)
fn graph_var_is_one_scan_node_whatever_the_graph_count<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let mut b: B = fixture();
    for i in 0..8 {
        let g = format!("http://ex/extra{i}");
        b.seed_quad(Some(&g), "http://ex/s", "http://ex/p", "http://ex/o");
    }
    let QueryAnswer::Explanation { text, .. } = execute_query_with(
        &format!("EXPLAIN {GRAPH_VAR_ALL}"),
        &b,
        &SparqlConfig::default(),
    )
    .unwrap() else {
        panic!("expected an explanation");
    };
    assert_eq!(
        text.matches("BgpScan").count(),
        1,
        "one scan node for ten graphs: {text}"
    );
    assert!(
        text.contains("[graph=?g]"),
        "the scan carries the graph scope: {text}"
    );
}

/// Reserved graphs stay out of the enumeration; naming one is the opt-in.
fn reserved_graphs_do_not_enumerate<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let mut b: B = fixture();
    b.seed_quad(Some(RESERVED), "http://ex/r", "http://ex/p", "http://ex/o4");
    assert_eq!(
        union_graph_rows(&b, GRAPH_VAR_ALL),
        all_graph_rows(),
        "a reserved graph is never enumerated by a bare GRAPH ?g"
    );
    assert_eq!(
        union_graph_rows(
            &b,
            &format!("SELECT ?g ?s FROM NAMED <{RESERVED}> WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}")
        ),
        vec![(RESERVED.to_owned(), "http://ex/r".to_owned())],
        "FROM NAMED on a reserved graph is the explicit opt-in"
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
    graph_var_enumerates_named_graphs_only,
    graph_var_binds_per_row,
    graph_var_restricted_by_from_named,
    graph_var_join_with_ground_var,
    select_star_projects_graph_var,
    graph_var_bound_by_the_pattern_itself,
    graph_var_count_counts_every_graph,
    graph_var_is_one_scan_node_whatever_the_graph_count,
    reserved_graphs_do_not_enumerate,
);
