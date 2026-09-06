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

/// Run `q` and return the IRIs bound to `?var`, sorted, **with duplicates
/// kept** (dedup bugs must be visible).
fn iris_bound_to<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
    var: &str,
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
        .map(|r| match r.get(var) {
            Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI-bound ?{var}, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// Run `q` and return the `?s` bindings, sorted, duplicates kept.
fn subjects<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
    mode: DefaultGraphMode,
) -> Vec<String> {
    iris_bound_to(store, q, "s", mode)
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

/// Run `q` under the default (`union`) mode and return its rows.
fn union_rows<E: horndb_sparql::exec::Executor + ?Sized>(
    store: &E,
    q: &str,
) -> Vec<horndb_sparql::exec::Bindings> {
    let QueryAnswer::Solutions { rows, .. } =
        execute_query_with(q, store, &SparqlConfig::default()).unwrap()
    else {
        panic!("expected solutions");
    };
    rows
}

/// The `(?a, ?b)` IRI pairs of `rows`, sorted, duplicates kept.
fn iri_pairs(rows: &[horndb_sparql::exec::Bindings], a: &str, b: &str) -> Vec<(String, String)> {
    let iri = |r: &horndb_sparql::exec::Bindings, v: &str| match r.get(v) {
        Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
        other => panic!("expected an IRI-bound ?{v}, got {other:?}"),
    };
    let mut out: Vec<(String, String)> = rows.iter().map(|r| (iri(r, a), iri(r, b))).collect();
    out.sort();
    out
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

/// Two rules keep the count seams from ever widening a scope (SPEC-28 S3).
///
/// **Decline by default**: a seam handed a scope it has not been taught
/// returns `Ok(None)`, which routes the caller to the scope-correct scan
/// fallback. `MemStore` implements neither seam and inherits that default.
///
/// **Refuse an unsubstituted graph variable**: `GRAPH ?g` is not one graph
/// set. The `PerGraph` operator substitutes the graph it is currently on
/// before any leaf scope is resolved, so a seam that still sees the
/// *variable* was reached outside that operator — a planner error, not a
/// number to guess at.
#[test]
fn count_seams_never_widen_an_unknown_scope() {
    use horndb_sparql::algebra::{DatasetSpec, GraphSpec, Term, TriplePattern, Var};
    use horndb_sparql::exec::{Executor, ScanScope};
    use horndb_sparql::plan::GraphScope;

    let patterns = [TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Var(Var::new("p")),
        object: Term::Var(Var::new("o")),
    }];
    let keys = [Var::new("s")];
    let graph_var = GraphScope::Named(GraphSpec::Var(Var::new("g")));
    let dataset = DatasetSpec::default();
    let scope = ScanScope::new(&graph_var, &dataset, DefaultGraphMode::Union);

    let horn: HornBackend = fixture();
    for err in [
        horn.count_bgp(&patterns, &scope).err(),
        horn.count_bgp_grouped(&patterns, &keys, &scope).err(),
    ] {
        let err = err
            .expect("an unsubstituted graph variable must not count")
            .to_string();
        assert!(
            err.contains("PerGraph"),
            "the refusal must name the missing PerGraph node: {err}"
        );
    }

    // A backend with no fast count declines under *every* scope, so the
    // scan fallback is the only path — the trait default is the decline.
    let mem: MemStore = fixture();
    assert_eq!(mem.count_bgp(&patterns, &ScanScope::DEFAULT).unwrap(), None);
    assert!(mem
        .count_bgp_grouped(&patterns, &keys, &ScanScope::DEFAULT)
        .unwrap()
        .is_none());
}

// --- property paths inherit the scope (SPEC-28 S3) ---------------------

/// A chain that walks out of `<g1>` and back in: `pa → pb` in g1,
/// `pb → pc` in g2, `pc → pd` in g1.
fn path_fixture<B: QuadSeed + Default>() -> B {
    let mut b = B::default();
    b.seed_quad(Some(G1), "http://ex/pa", "http://ex/link", "http://ex/pb");
    b.seed_quad(Some(G2), "http://ex/pb", "http://ex/link", "http://ex/pc");
    b.seed_quad(Some(G1), "http://ex/pc", "http://ex/link", "http://ex/pd");
    b
}

const PATH_FROM_PA: &str = "SELECT ?y WHERE { <http://ex/pa> <http://ex/link>+ ?y }";

/// The scope is applied to the closure's **edge relation**, not to its
/// output: inside `GRAPH <g1>` only g1's two edges exist, so `pa` reaches
/// `pb` and stops. Post-filtering an all-graphs closure would connect
/// `pa → pd` through g2's hop — a different, wrong answer.
fn path_scope_applied_before_closure<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = path_fixture();
    assert_eq!(
        iris_bound_to(
            &b,
            &format!(
                "SELECT ?y WHERE {{ GRAPH <{G1}> {{ <http://ex/pa> <http://ex/link>+ ?y }} }}"
            ),
            "y",
            DefaultGraphMode::Union,
        ),
        vec!["http://ex/pb"],
        "the closure must run over g1's edges only — pc/pd are reachable \
         only through g2"
    );
}

/// The other half of the pair: with no `GRAPH` wrapper the union default
/// graph *is* every non-reserved graph, so the same chain legitimately
/// traverses all three hops. Without this arm the test above would pass on
/// a closure that simply returned nothing.
fn path_over_union_traverses_graphs<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = path_fixture();
    assert_eq!(
        iris_bound_to(&b, PATH_FROM_PA, "y", DefaultGraphMode::Union),
        vec!["http://ex/pb", "http://ex/pc", "http://ex/pd"],
        "the union default graph holds every hop, so the chain connects"
    );
    // …and `strict` mode, whose default graph holds none of these quads,
    // connects nothing — the mode reaches the path's edge relation too.
    assert!(
        iris_bound_to(&b, PATH_FROM_PA, "y", DefaultGraphMode::Strict).is_empty(),
        "strict mode sees only the default-graph sentinel, which is empty here"
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
    assert_eq!(
        text.matches("PerGraph(?g)").count(),
        1,
        "one PerGraph node for ten graphs — the loop is in the operator: {text}"
    );
}

/// Shapes inside `GRAPH ?g` that a scan-leaf graph column could not carry:
/// a closure path, the `Distinct(Project(..))` wrapper the translator puts
/// around an alternative or negated path, a globally-truncating existence
/// path, an aggregating sub-`SELECT`, and a nested ground `GRAPH`.
///
/// Per-graph block evaluation (SPEC-28 S3/D6) answers all of them: the block
/// runs once per named graph, so each shape sees exactly one graph's
/// triples, and `?g` is joined on afterwards.
fn complex_shapes_inside_graph_var_answer_per_graph<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let mut b: B = fixture();
    b.seed_quad(Some(G1), "http://ex/x", "http://ex/q", "http://ex/y");
    b.seed_quad(Some(G2), "http://ex/y", "http://ex/q", "http://ex/z");

    // The closure runs over one graph's edges, so the cross-graph `x → z`
    // (a hop in g1 joined to a hop in g2) is never derived.
    assert_eq!(
        iri_pairs(
            &union_rows(
                &b,
                "SELECT ?g ?x ?y WHERE { GRAPH ?g { ?x <http://ex/q>+ ?y } }"
            ),
            "g",
            "y"
        ),
        vec![
            (G1.to_owned(), "http://ex/y".to_owned()),
            (G2.to_owned(), "http://ex/z".to_owned()),
        ],
        "a closure path stays inside one graph"
    );

    // An alternative path and a negated property set both keep every graph's
    // rows: the `Distinct` inside the block dedups within one graph, so g1's
    // and g2's copies of t2 no longer collapse into one row.
    let every_triple = vec![
        (G1.to_owned(), "http://ex/b".to_owned()),
        (G1.to_owned(), "http://ex/x".to_owned()),
        (G2.to_owned(), "http://ex/b".to_owned()),
        (G2.to_owned(), "http://ex/c".to_owned()),
        (G2.to_owned(), "http://ex/y".to_owned()),
    ];
    for q in [
        "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s <http://ex/p>|<http://ex/q> ?o } }",
        "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s !(<http://ex/zzz>) ?o } }",
    ] {
        assert_eq!(
            iri_pairs(&union_rows(&b, q), "g", "s"),
            every_triple,
            "every graph's triples, deduped within the graph: {q}"
        );
    }

    // A ground-endpoint path is an existence probe. It is truncated once per
    // graph, so t2 being in both graphs gives two rows, not one.
    assert_eq!(
        iris_bound_to(
            &b,
            "SELECT ?g WHERE { GRAPH ?g { <http://ex/b> <http://ex/p>|<http://ex/q> \
             <http://ex/o2> } }",
            "g",
            DefaultGraphMode::Union
        ),
        vec![G1.to_owned(), G2.to_owned()],
        "existence is decided once per graph"
    );

    // An aggregating sub-SELECT counts one graph at a time.
    let mut counted: Vec<(String, String)> = union_rows(
        &b,
        "SELECT ?g ?c WHERE { GRAPH ?g { { SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o } } } }",
    )
    .iter()
    .map(|r| {
        let g = match r.get("g") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI-bound ?g, got {other:?}"),
        };
        (g, format!("{:?}", r.get("c")))
    })
    .collect();
    counted.sort();
    assert_eq!(counted.len(), 2, "one count per graph: {counted:?}");
    assert_eq!(counted[0].0, G1);
    assert!(
        counted[0].1.contains('2'),
        "g1 holds two triples: {counted:?}"
    );
    assert_eq!(counted[1].0, G2);
    assert!(
        counted[1].1.contains('3'),
        "g2 holds three triples: {counted:?}"
    );

    // A nested ground `GRAPH` scopes its own leaves; the outer `?g` still
    // enumerates every named graph, so g1's rows come back once per graph.
    assert_eq!(
        iri_pairs(
            &union_rows(
                &b,
                "SELECT ?g ?s WHERE { GRAPH ?g { GRAPH <http://ex/g1> { ?s ?p ?o } } }"
            ),
            "g",
            "s"
        ),
        vec![
            (G1.to_owned(), "http://ex/b".to_owned()),
            (G1.to_owned(), "http://ex/x".to_owned()),
            (G2.to_owned(), "http://ex/b".to_owned()),
            (G2.to_owned(), "http://ex/x".to_owned()),
        ],
        "the inner scope picks the triples, the outer variable picks the graph"
    );
}

/// `VALUES` is legal and correctly evaluable inside `GRAPH ?g`: it reads no
/// quads, so the scoped BGP it joins against supplies the graph column on
/// every output row. It must answer, not refuse.
fn values_inside_graph_var_answers<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o VALUES ?o { <http://ex/o2> } } }"
        ),
        vec![
            (G1.to_owned(), "http://ex/b".to_owned()),
            (G2.to_owned(), "http://ex/b".to_owned()),
        ],
        "a quad-free join arm keeps the graph column"
    );
}

/// SPARQL 1.1 §18.2.2.2 evaluates `GRAPH ?g { P }` with `?g` **free** inside
/// `P` and joins the graph name on afterwards. Per-graph block evaluation
/// does exactly that, so a read of `?g` *inside* the block sees an unbound
/// variable — the graph name arrives only in the post-join.
///
/// These are the W3C `graph-variable-scope` and `graph-optional` shapes.
/// Binding `?g` on the scan leaf instead used to give 2 and 4 rows here.
fn graph_var_is_free_inside_the_block<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let mut b: B = fixture();

    // W3C graph-variable-scope: `BOUND(?g)` is false inside the block.
    assert!(
        union_rows(&b, "SELECT * WHERE { GRAPH ?g { FILTER(BOUND(?g)) } }").is_empty(),
        "?g is free inside the block, so BOUND(?g) is false"
    );

    // The shape a user is most likely to write. The FILTER sees `?g`
    // unbound, so it errors and drops every row — the answer is zero rows,
    // not "the rows from <g1>".
    assert!(
        union_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o FILTER(?g = <http://ex/g1>) } }"
        )
        .is_empty(),
        "a FILTER on ?g inside the block cannot select a graph"
    );

    // An OPTIONAL condition reading `?g` is the same case: the right arm
    // errors on the unbound variable, so every left row survives with the
    // optional part unbound.
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o \
             OPTIONAL { ?s ?p ?o2 FILTER(?o2 = ?g) } } }"
        ),
        all_graph_rows(),
        "an unsatisfiable OPTIONAL leaves the left rows intact"
    );

    // BIND of the free `?g` produces an unbound `?x`, not the graph name.
    let bound = union_rows(
        &b,
        "SELECT ?g ?x WHERE { GRAPH ?g { ?s ?p ?o BIND(?g AS ?x) } }",
    );
    assert_eq!(bound.len(), 3, "one row per quad in a named graph");
    assert!(
        bound.iter().all(|r| r.get("x").is_none()),
        "BIND(?g AS ?x) inside the block binds nothing: {bound:?}"
    );

    // W3C graph-optional: the OPTIONAL binds `?g` from the object, and the
    // post-join then keeps only the rows whose object *is* the graph name.
    // No object in the base fixture is a graph IRI, so nothing survives.
    assert!(
        union_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o OPTIONAL { ?s ?p ?g } } }"
        )
        .is_empty(),
        "no object names its own graph, so the post-join keeps nothing"
    );
    // Give g1 a triple whose object *is* `<g1>` and the post-join keeps it.
    b.seed_quad(Some(G1), "http://ex/b", "http://ex/p", G1);
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o OPTIONAL { ?s ?p ?g } } }"
        ),
        vec![
            (G1.to_owned(), "http://ex/b".to_owned()),
            (G1.to_owned(), "http://ex/b".to_owned()),
        ],
        "the two g1 rows whose OPTIONAL bound ?g to <g1> survive the post-join"
    );
}

/// The equivalence boundary of that rule: an `OPTIONAL` whose right arm never
/// mentions `?g` keeps working, and so does `?g` in a plain pattern position
/// (leaf-binding *is* the post-join there).
fn graph_var_beside_an_optional_still_answers<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let mut b: B = fixture();
    b.seed_quad(Some(G1), G1, "http://ex/label", "http://ex/l");
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?g <http://ex/label> ?o \
             OPTIONAL { ?s <http://ex/p> ?o2 } } }"
        ),
        vec![(G1.to_owned(), "http://ex/b".to_owned())],
        "an OPTIONAL that does not mention ?g leaves the equivalence intact"
    );
}

/// The boundary the refusal must not cross: `DISTINCT`, `LIMIT` and the
/// projection of the enclosing `SELECT` sit **above** the `GRAPH` node, so
/// they keep working — only barriers *inside* the graph pattern refuse.
fn distinct_and_limit_above_graph_var_still_answer<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    assert_eq!(
        union_graph_rows(&b, "SELECT DISTINCT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o } }"),
        all_graph_rows(),
        "top-level DISTINCT is outside the GRAPH subtree"
    );
    assert_eq!(
        union_graph_rows(
            &b,
            "SELECT ?g ?s WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY ?g ?s LIMIT 2"
        ),
        all_graph_rows()[..2].to_vec(),
        "top-level ORDER BY + LIMIT is outside the GRAPH subtree"
    );
}

/// `FROM <g>` with no `FROM NAMED` sets an **empty** named set, so `GRAPH ?g`
/// enumerates nothing — the trap in `DatasetSpec`'s "any clause ⇒ both fields
/// `Some`" invariant, pinned end to end here as well as in `scope.rs`.
fn from_only_leaves_no_graphs_to_enumerate<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    assert!(
        union_graph_rows(
            &b,
            &format!("SELECT ?g ?s FROM <{G1}> WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}")
        )
        .is_empty(),
        "FROM without FROM NAMED leaves an empty named set (SPARQL 1.1 §13.2)"
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

/// `ASK { GRAPH <g> {} }` is the standard graph-existence probe: an empty
/// group has no quad to scan, so the whole answer *is* "does the dataset have
/// `g`" (SPARQL 1.1 §18.2.2.4 evaluates `Graph(g, P)` only for
/// `g ∈ names(D)`).
///
/// Both backends used to take a zero-pattern shortcut that emitted the unit
/// row before consulting the scope, so the probe answered `true` for every
/// IRI — a silent wrong answer at HTTP 200 (SPEC-28 D1). The W3C fixtures
/// alone would not hold this: `graph-exist` passes either way, and only
/// `graph-not-exist` catches it. Hence this direct pin.
fn empty_group_probes_graph_existence<B: QuadSeed + Default + horndb_sparql::exec::Executor>() {
    let b: B = fixture();
    let ask = |q: &str| match execute_query_with(q, &b, &SparqlConfig::default()) {
        Ok(QueryAnswer::Boolean(v)) => v,
        other => panic!("expected a boolean for {q}: {other:?}"),
    };
    assert!(
        ask(&format!("ASK {{ GRAPH <{G1}> {{}} }}")),
        "an existing graph makes the empty group match"
    );
    assert!(
        !ask("ASK { GRAPH <http://ex/nope> {} }"),
        "a graph the dataset does not have must NOT match the empty group"
    );
    // The default graph always exists — an empty group outside any GRAPH
    // still matches, and so does one inside a *variable* GRAPH over the
    // graphs that do exist.
    assert!(ask("ASK {}"), "the default graph always matches `{{}}`");
    assert_eq!(
        iris_bound_to(
            &b,
            "SELECT ?g WHERE { GRAPH ?g {} }",
            "g",
            DefaultGraphMode::Union
        ),
        vec![G1.to_owned(), G2.to_owned()],
        "GRAPH ?g {{}} enumerates exactly the graphs that exist (W3C graph-empty)"
    );
    // A ground graph outside the dataset's named set does not exist *for
    // this query*, even though the store holds it.
    assert!(
        !ask(&format!("ASK FROM NAMED <{G2}> {{ GRAPH <{G1}> {{}} }}")),
        "FROM NAMED excludes g1, so the probe must answer false"
    );
}

/// Regression (HDB-74): `PerGraph` builds one operator tree per graph, so
/// each graph decides its column provenance (`Slot::Id` vs `Slot::Term`)
/// from its own data. Here the `OPTIONAL`'s right side matches in `g2` only,
/// and binds `?o` from `VALUES` — so `?o` used to arrive as an id from `g1`
/// and as a term from `g2`. That breaks the stream-wide column-homogeneity
/// invariant every consumer keys on: `DISTINCT` counted the same IRI twice,
/// and `GROUP BY`'s scalar fast path hit its `unreachable!`. Only the horn
/// backend can show it — `MemStore` yields terms everywhere.
fn per_graph_columns_are_homogeneous_across_graphs<
    B: QuadSeed + Default + horndb_sparql::exec::Executor,
>() {
    let b: B = fixture();
    const BLOCK: &str = "GRAPH ?g { ?s <http://ex/p> ?o \
        OPTIONAL { ?x <http://ex/p> <http://ex/o3> VALUES ?o { <http://ex/o2> } } }";
    assert_eq!(
        iris_bound_to(
            &b,
            &format!("SELECT DISTINCT ?o WHERE {{ {BLOCK} }}"),
            "o",
            DefaultGraphMode::Union
        ),
        vec!["http://ex/o2".to_owned(), "http://ex/o3".to_owned()],
        "DISTINCT must see one row per term, not one per column encoding"
    );
    let rows = union_rows(
        &b,
        &format!("SELECT ?o (COUNT(*) AS ?n) WHERE {{ {BLOCK} }} GROUP BY ?o"),
    );
    assert_eq!(rows.len(), 2, "one group per distinct ?o: {rows:?}");
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
    path_scope_applied_before_closure,
    path_over_union_traverses_graphs,
    graph_var_enumerates_named_graphs_only,
    graph_var_binds_per_row,
    graph_var_restricted_by_from_named,
    graph_var_join_with_ground_var,
    select_star_projects_graph_var,
    graph_var_bound_by_the_pattern_itself,
    graph_var_count_counts_every_graph,
    graph_var_is_one_scan_node_whatever_the_graph_count,
    complex_shapes_inside_graph_var_answer_per_graph,
    values_inside_graph_var_answers,
    graph_var_is_free_inside_the_block,
    graph_var_beside_an_optional_still_answers,
    distinct_and_limit_above_graph_var_still_answer,
    from_only_leaves_no_graphs_to_enumerate,
    reserved_graphs_do_not_enumerate,
    empty_group_probes_graph_existence,
    per_graph_columns_are_homogeneous_across_graphs,
);
