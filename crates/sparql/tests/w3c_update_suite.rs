//! Drives the W3C SPARQL 1.1 Update graph families committed in
//! `crates/harness/tests/fixtures/sparql11/update_selected_subset/`
//! (SPEC-28 S7 phase 4, #267): `add/`, `copy/`, `move/`, `clear/`, `drop/`,
//! and the graph-specific `delete/` family (`WITH`/`USING`/`USING NAMED`,
//! ground `GRAPH` blocks). The case list mirrors `harness/selected.toml`'s
//! `[sparql_update]` section — see that file's comment for why the upstream
//! `delete-insert/` directory is not the source (it has no graph-specific
//! eval cases; the real "Graph-specific DELETE" cases live in `delete/`).
//!
//! Each case dir carries `request.ru` (the raw SPARQL Update request),
//! `data.trig` (every graph's initial content) and `expected.trig` (every
//! graph's final content). The runner loads `data.trig`, applies
//! `request.ru` via [`apply_update`], and compares the resulting store's
//! FULL quad set (default graph + every named graph) against
//! `expected.trig` by set equality — matching the upstream test runner's
//! "unmentioned graph is empty" convention baked into the fixtures
//! (SPEC-28 D11: empty and absent are the same fact for a quad-set compare).
//!
//! Two backends, same as `w3c_suite.rs`: [`MemStore`] and [`HornBackend`].
//!
//! 47 of 47 upstream cases across the six families are mirrored as fixtures;
//! 45 are selected here (both backends green). `dawg-delete-with-02` and
//! `dawg-delete-with-06` are excluded — not a D11 gap, but a real dataset-
//! scoping bug (a bare `WITH` wrongly zeroes named-graph visibility for
//! ground `GRAPH` blocks in WHERE) tracked as issue #281 and documented in
//! `harness/KNOWN-MANIFEST-BUGS.md`.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, GraphNamedNode, Store, StoreGraphTarget};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/update_selected_subset");
    p
}

/// Lower an `oxrdf` subject to an algebra [`Term`] — mirrors
/// `update.rs::oxrdf_subject_to_term` (private to that module, so this test
/// binary — a separate compilation unit — carries its own copy, same as
/// `w3c_suite.rs` does for its lexical conversions).
fn oxrdf_subject_to_term(s: &oxrdf::NamedOrBlankNode) -> Term {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

/// Lower an `oxrdf` object term to an algebra [`Term`]. Mirrors
/// `update.rs::oxrdf_term_to_term`.
fn oxrdf_term_to_term(t: &oxrdf::Term) -> Term {
    match t {
        oxrdf::Term::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::Term::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        oxrdf::Term::Literal(l) => Term::Literal(l.to_string()),
        oxrdf::Term::Triple(tr) => Term::Literal(tr.to_string()),
    }
}

/// Seed a store from a `data.trig` fixture: one [`Store::apply_quads`] batch
/// carrying every quad in the document, routed to its own graph.
fn load_trig<B: Store>(path: &Path, store: &mut B) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut adds = Vec::new();
    for q in oxttl::TriGParser::new().for_slice(&bytes) {
        let q = q.unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        let graph = match &q.graph_name {
            oxrdf::GraphName::DefaultGraph => None,
            oxrdf::GraphName::NamedNode(g) => Some(Term::Iri(g.as_str().to_owned())),
            oxrdf::GraphName::BlankNode(_) => panic!("blank-node graph names are not used"),
        };
        adds.push((
            graph,
            oxrdf_subject_to_term(&q.subject),
            Term::Iri(q.predicate.as_str().to_owned()),
            oxrdf_term_to_term(&q.object),
        ));
    }
    store.apply_quads(Vec::new(), adds).expect("seed data.trig");
}

/// Canonical printable key for a ground term, for quad-set comparison. Every
/// term in these fixtures is ground (no variables, no RDF 1.2 triple terms).
fn term_key(t: &Term) -> String {
    match t {
        Term::Iri(s) => format!("<{s}>"),
        Term::Literal(s) => s.clone(),
        Term::BlankNode(s) => format!("_:{s}"),
        other => panic!("unexpected non-ground term in an update fixture: {other:?}"),
    }
}

fn quad_line(graph: Option<&str>, s: &Term, p: &Term, o: &Term) -> String {
    let triple = format!("{} {} {} .", term_key(s), term_key(p), term_key(o));
    match graph {
        None => triple,
        Some(g) => format!("GRAPH <{g}> {{ {triple} }}"),
    }
}

/// Every quad in `store` (default graph + every named graph), as a
/// comparable set of canonical strings.
fn dump_store<B: FullBackend>(store: &B) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (s, p, o) in store
        .scan_graph_quads(&StoreGraphTarget::DefaultGraph)
        .unwrap()
    {
        out.insert(quad_line(None, &s, &p, &o));
    }
    for g in Store::named_graphs(store) {
        let tgt = StoreGraphTarget::NamedNode(GraphNamedNode::new_unchecked(&g));
        for (s, p, o) in store.scan_graph_quads(&tgt).unwrap() {
            out.insert(quad_line(Some(&g), &s, &p, &o));
        }
    }
    out
}

/// Every quad named in an `expected.trig` fixture, as the same comparable
/// set of canonical strings `dump_store` produces.
fn expected_quad_set(path: &Path) -> BTreeSet<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = BTreeSet::new();
    for q in oxttl::TriGParser::new().for_slice(&bytes) {
        let q = q.unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        let s = oxrdf_subject_to_term(&q.subject);
        let p = Term::Iri(q.predicate.as_str().to_owned());
        let o = oxrdf_term_to_term(&q.object);
        let graph = match &q.graph_name {
            oxrdf::GraphName::DefaultGraph => None,
            oxrdf::GraphName::NamedNode(g) => Some(g.as_str().to_owned()),
            oxrdf::GraphName::BlankNode(_) => panic!("blank-node graph names are not used"),
        };
        out.insert(quad_line(graph.as_deref(), &s, &p, &o));
    }
    out
}

fn run_one<B: FullBackend + Default>(name: &str) {
    let dir = fixtures_root().join(name);
    let mut store = B::default();
    load_trig(&dir.join("data.trig"), &mut store);

    let req_text = std::fs::read_to_string(dir.join("request.ru")).expect("read request.ru");
    let parsed = parse_update(&req_text).unwrap_or_else(|e| panic!("{name}: parse: {e}"));
    apply_update(&parsed, &mut store).unwrap_or_else(|e| panic!("{name}: apply: {e}"));

    let got = dump_store(&store);
    let want = expected_quad_set(&dir.join("expected.trig"));
    assert_eq!(got, want, "{name}: final store state differs");
}

// ── MemStore leg ──────────────────────────────────────────────────────────

macro_rules! update_case {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<MemStore>($dir);
        }
    };
}

// W3C SPARQL 1.1 `add/` family (SPEC-28 S4, #267) — `ADD [SILENT] src TO dst`
// between named graphs, DEFAULT, and self. Mirrored from
// https://www.w3.org/2009/sparql/docs/tests/sparql11-test-suite-20121023.tar.gz .
update_case!(add01, "add01");
update_case!(add02, "add02");
update_case!(add03, "add03");
update_case!(add04, "add04");
update_case!(add05, "add05");
update_case!(add06, "add06");
update_case!(add07, "add07");
update_case!(add08, "add08");

// `copy/` family — `COPY [SILENT] src TO dst`.
update_case!(copy01, "copy01");
update_case!(copy02, "copy02");
update_case!(copy03, "copy03");
update_case!(copy04, "copy04");
update_case!(copy06, "copy06");
update_case!(copy07, "copy07");

// `move/` family — `MOVE [SILENT] src TO dst`.
update_case!(move01, "move01");
update_case!(move02, "move02");
update_case!(move03, "move03");
update_case!(move04, "move04");
update_case!(move06, "move06");
update_case!(move07, "move07");

// `clear/` family — `CLEAR (DEFAULT|GRAPH g|NAMED|ALL)`.
update_case!(dawg_clear_default_01, "dawg-clear-default-01");
update_case!(dawg_clear_graph_01, "dawg-clear-graph-01");
update_case!(dawg_clear_named_01, "dawg-clear-named-01");
update_case!(dawg_clear_all_01, "dawg-clear-all-01");

// `drop/` family — `DROP (DEFAULT|GRAPH g|NAMED|ALL)`.
update_case!(dawg_drop_default_01, "dawg-drop-default-01");
update_case!(dawg_drop_graph_01, "dawg-drop-graph-01");
update_case!(dawg_drop_named_01, "dawg-drop-named-01");
update_case!(dawg_drop_all_01, "dawg-drop-all-01");

// `delete/` family — the graph-specific `DELETE ... WHERE` family: ground
// `GRAPH <g> {}` blocks in the DELETE/WHERE templates, `WITH <g>`, `USING
// <g>`, `USING NAMED <g>` (SPEC-28 S3/S4).
update_case!(dawg_delete_01, "dawg-delete-01");
update_case!(dawg_delete_02, "dawg-delete-02");
update_case!(dawg_delete_03, "dawg-delete-03");
update_case!(dawg_delete_04, "dawg-delete-04");
update_case!(dawg_delete_05, "dawg-delete-05");
update_case!(dawg_delete_06, "dawg-delete-06");
update_case!(dawg_delete_07, "dawg-delete-07");
update_case!(dawg_delete_with_01, "dawg-delete-with-01");
// dawg-delete-with-02 excluded: issue #281 (bare WITH zeroes named-graph
// visibility for a ground GRAPH block in WHERE) — see KNOWN-MANIFEST-BUGS.md.
update_case!(dawg_delete_with_03, "dawg-delete-with-03");
update_case!(dawg_delete_with_04, "dawg-delete-with-04");
update_case!(dawg_delete_with_05, "dawg-delete-with-05");
// dawg-delete-with-06 excluded: same issue #281.
update_case!(dawg_delete_using_01, "dawg-delete-using-01");
update_case!(dawg_delete_using_02a, "dawg-delete-using-02a");
update_case!(dawg_delete_using_03, "dawg-delete-using-03");
update_case!(dawg_delete_using_04, "dawg-delete-using-04");
update_case!(dawg_delete_using_05, "dawg-delete-using-05");
update_case!(dawg_delete_using_06a, "dawg-delete-using-06a");

// ── HornBackend leg ───────────────────────────────────────────────────────

macro_rules! update_case_horn {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<HornBackend>($dir);
        }
    };
}

update_case_horn!(add01_hornbackend, "add01");
update_case_horn!(add02_hornbackend, "add02");
update_case_horn!(add03_hornbackend, "add03");
update_case_horn!(add04_hornbackend, "add04");
update_case_horn!(add05_hornbackend, "add05");
update_case_horn!(add06_hornbackend, "add06");
update_case_horn!(add07_hornbackend, "add07");
update_case_horn!(add08_hornbackend, "add08");

update_case_horn!(copy01_hornbackend, "copy01");
update_case_horn!(copy02_hornbackend, "copy02");
update_case_horn!(copy03_hornbackend, "copy03");
update_case_horn!(copy04_hornbackend, "copy04");
update_case_horn!(copy06_hornbackend, "copy06");
update_case_horn!(copy07_hornbackend, "copy07");

update_case_horn!(move01_hornbackend, "move01");
update_case_horn!(move02_hornbackend, "move02");
update_case_horn!(move03_hornbackend, "move03");
update_case_horn!(move04_hornbackend, "move04");
update_case_horn!(move06_hornbackend, "move06");
update_case_horn!(move07_hornbackend, "move07");

update_case_horn!(dawg_clear_default_01_hornbackend, "dawg-clear-default-01");
update_case_horn!(dawg_clear_graph_01_hornbackend, "dawg-clear-graph-01");
update_case_horn!(dawg_clear_named_01_hornbackend, "dawg-clear-named-01");
update_case_horn!(dawg_clear_all_01_hornbackend, "dawg-clear-all-01");

update_case_horn!(dawg_drop_default_01_hornbackend, "dawg-drop-default-01");
update_case_horn!(dawg_drop_graph_01_hornbackend, "dawg-drop-graph-01");
update_case_horn!(dawg_drop_named_01_hornbackend, "dawg-drop-named-01");
update_case_horn!(dawg_drop_all_01_hornbackend, "dawg-drop-all-01");

update_case_horn!(dawg_delete_01_hornbackend, "dawg-delete-01");
update_case_horn!(dawg_delete_02_hornbackend, "dawg-delete-02");
update_case_horn!(dawg_delete_03_hornbackend, "dawg-delete-03");
update_case_horn!(dawg_delete_04_hornbackend, "dawg-delete-04");
update_case_horn!(dawg_delete_05_hornbackend, "dawg-delete-05");
update_case_horn!(dawg_delete_06_hornbackend, "dawg-delete-06");
update_case_horn!(dawg_delete_07_hornbackend, "dawg-delete-07");
update_case_horn!(dawg_delete_with_01_hornbackend, "dawg-delete-with-01");
// dawg-delete-with-02 excluded: issue #281 (see the MemStore leg above).
update_case_horn!(dawg_delete_with_03_hornbackend, "dawg-delete-with-03");
update_case_horn!(dawg_delete_with_04_hornbackend, "dawg-delete-with-04");
update_case_horn!(dawg_delete_with_05_hornbackend, "dawg-delete-with-05");
// dawg-delete-with-06 excluded: issue #281.
update_case_horn!(dawg_delete_using_01_hornbackend, "dawg-delete-using-01");
update_case_horn!(dawg_delete_using_02a_hornbackend, "dawg-delete-using-02a");
update_case_horn!(dawg_delete_using_03_hornbackend, "dawg-delete-using-03");
update_case_horn!(dawg_delete_using_04_hornbackend, "dawg-delete-using-04");
update_case_horn!(dawg_delete_using_05_hornbackend, "dawg-delete-using-05");
update_case_horn!(dawg_delete_using_06a_hornbackend, "dawg-delete-using-06a");
