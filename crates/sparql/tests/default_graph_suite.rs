//! Grades the **shipped `default_graph = union` mode** (SPEC-28 D2) against a
//! fixture family, the way `w3c_suite.rs` grades the W3C query subset.
//!
//! **These fixtures are HornDB-specific, not W3C cases.** SPARQL 1.1 §13.2
//! leaves the no-`FROM` dataset implementation-defined, so no upstream case
//! can grade it: the W3C `graph/` family fixes its dataset in the manifest
//! (those mirrors run `strict`) and the `dataset/` family carries explicit
//! `FROM` / `FROM NAMED`, which wins over the mode. Before this family the
//! shipped default was covered only by `tests/graph_query.rs` unit tests —
//! see `harness/KNOWN-MANIFEST-BUGS.md`.
//!
//! The case list is `harness/selected.toml`'s `[sparql_default_graph].tests`
//! and the fixtures live in
//! `crates/harness/tests/fixtures/sparql11/default_graph_subset/`. The runner
//! reads that list rather than repeating it, and asserts it matches the dirs
//! on disk, so a fixture cannot be added without being graded (nor listed
//! without existing). Each case carries `data.trig`, `query.rq`, `form` and
//! `expected.srj` — no `default-graph` file, because the whole family runs
//! [`DefaultGraphMode::Union`] by construction.
//!
//! Both Stage-1 backends run every case: [`MemStore`] and [`HornBackend`].
//!
//! [`union_family_is_mode_sensitive`] is the family's own negative control:
//! it re-runs every case under `strict` and asserts each one now *differs*
//! from `expected.srj`. A case that passed under both modes would grade
//! nothing, which is the defect this family exists to fix.

use horndb_sparql::api::{execute_query_with, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::results::json::{write_ask_json, write_select_json};
use horndb_sparql::{DefaultGraphMode, SparqlConfig};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Manifest path prefix of every case in this family.
const SUBSET: &str = "default_graph_subset/";

fn workspace_root() -> PathBuf {
    // tests live in crates/sparql/tests/; the workspace root is two up.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // workspace root
    p
}

fn fixtures_root() -> PathBuf {
    workspace_root().join("crates/harness/tests/fixtures/sparql11/default_graph_subset")
}

/// Just enough of `harness/selected.toml` to read `[sparql_default_graph]`.
/// A local subset rather than a dependency on `horndb-harness`: that crate
/// sits on top of `horndb-sparql` in the workspace's dependency order, so
/// depending on it here would invert the layering. Mirrors the same trick in
/// `w3c_suite.rs`.
#[derive(serde::Deserialize)]
struct SelectedManifest {
    sparql_default_graph: Section,
}

#[derive(serde::Deserialize)]
struct Section {
    tests: Vec<String>,
}

/// The case directory names `[sparql_default_graph].tests` selects.
fn selected_cases() -> Vec<String> {
    let path = workspace_root().join("harness/selected.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: SelectedManifest =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    doc.sparql_default_graph
        .tests
        .iter()
        .map(|t| {
            t.strip_prefix(SUBSET)
                .unwrap_or_else(|| {
                    panic!("[sparql_default_graph].tests entry {t:?} not under {SUBSET}")
                })
                .to_owned()
        })
        .collect()
}

// ── Fixture loading ──────────────────────────────────────────────────────────

/// Seed one quad into a backend. `None` is the default graph.
///
/// Seeding named graphs goes through each backend's storage seam rather than
/// the SPARQL Update policy layer, so a fixture can plant quads in any graph
/// directly — including a reserved `https://horndb.io/graph/…` one, which the
/// update path refuses. Same shape as `w3c_suite.rs` and `w3c_update_suite.rs`.
trait QuadSeed {
    fn seed_quad(&mut self, graph: Option<&str>, s: &str, p: &str, o: &str);
}

impl QuadSeed for MemStore {
    fn seed_quad(&mut self, graph: Option<&str>, s: &str, p: &str, o: &str) {
        // `MemStore` keeps IRIs bare (`term_to_lex` in `exec/mem.rs`).
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

/// Load a case's `data.trig`. Every term in this family is an IRI — the modes
/// differ on *which graphs* are in the default graph, not on term shapes — so
/// a non-IRI term is a fixture bug, not a case to grade.
fn load_trig<B: QuadSeed + Default>(path: &Path) -> B {
    let mut b = B::default();
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for q in oxttl::TriGParser::new().for_slice(&bytes) {
        let q = q.unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        let graph = match &q.graph_name {
            oxrdf::GraphName::DefaultGraph => None,
            oxrdf::GraphName::NamedNode(g) => Some(g.as_str().to_owned()),
            other => panic!("{}: unsupported graph name {other}", path.display()),
        };
        let as_iri = |t: oxrdf::Term| match t {
            oxrdf::Term::NamedNode(n) => n.into_string(),
            other => panic!(
                "{}: fixtures use IRI terms only, got {other}",
                path.display()
            ),
        };
        let s = as_iri(q.subject.into());
        let o = as_iri(q.object);
        b.seed_quad(graph.as_deref(), &s, q.predicate.as_str(), &o);
    }
    b
}

// ── Running one case ─────────────────────────────────────────────────────────

/// Run `case` under `mode` and return the engine's result document.
fn answer<B: FullBackend + QuadSeed + Default>(case: &str, mode: DefaultGraphMode) -> String {
    let dir = fixtures_root().join(case);
    let backend: B = load_trig(&dir.join("data.trig"));
    let q = std::fs::read_to_string(dir.join("query.rq")).expect("read query.rq");
    let form = std::fs::read_to_string(dir.join("form"))
        .expect("read form")
        .trim()
        .to_owned();
    let cfg = SparqlConfig {
        default_graph: mode,
        ..SparqlConfig::default()
    };
    let ans = execute_query_with(&q, &backend, &cfg).unwrap_or_else(|e| panic!("{case}: {e}"));
    match (form.as_str(), ans) {
        ("select", QueryAnswer::Solutions { vars, rows }) => write_select_json(&vars, &rows),
        ("ask", QueryAnswer::Boolean(b)) => write_ask_json(b),
        (form, ans) => panic!("{case}: unexpected form/answer pair {form:?} / {ans:?}"),
    }
}

/// Canonical form of a SPARQL-JSON result: the variable set plus the sorted
/// multiset of bindings (an ASK answer's `boolean` survives as-is). Comparing
/// these strings is order-insensitive but duplicate-sensitive — a union
/// default graph is a *set* union, so a triple in two graphs must come back
/// once, and a dedup bug has to be visible.
fn canonical(doc: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(doc).unwrap();
    let vars: BTreeSet<&str> = v["head"]["vars"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let mut rows: Vec<String> = v["results"]["bindings"]
        .as_array()
        .map(|a| a.iter().map(|b| b.to_string()).collect())
        .unwrap_or_default();
    rows.sort();
    format!("vars={vars:?} boolean={} rows={rows:?}", v["boolean"])
}

fn expected(case: &str) -> String {
    let path = fixtures_root().join(case).join("expected.srj");
    canonical(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}")))
}

fn run_all<B: FullBackend + QuadSeed + Default>(backend_name: &str) {
    for case in selected_cases() {
        let got = canonical(&answer::<B>(&case, DefaultGraphMode::Union));
        assert_eq!(
            got,
            expected(&case),
            "{backend_name}/{case}: result differs from expected.srj under union mode"
        );
    }
}

#[test]
fn union_family_memstore() {
    run_all::<MemStore>("MemStore");
}

#[test]
fn union_family_hornbackend() {
    run_all::<HornBackend>("HornBackend");
}

// ── The family's own guards ──────────────────────────────────────────────────

/// Negative control: every case must *fail* under `strict`.
///
/// Without this, a case that happens to be mode-insensitive would sit in the
/// manifest looking like coverage while grading nothing — the exact reason
/// the W3C `graph/` and `dataset/` families cannot gate the shipped default.
#[test]
fn union_family_is_mode_sensitive() {
    for case in selected_cases() {
        let strict = canonical(&answer::<HornBackend>(&case, DefaultGraphMode::Strict));
        assert_ne!(
            strict,
            expected(&case),
            "{case}: matches expected.srj under strict too, so it does not grade \
             default_graph = union. Either make the fixture mode-sensitive or drop it."
        );
    }
}

/// The manifest list and the fixture tree must agree in both directions: a
/// fixture not listed is never graded, and a listed case with no fixture is a
/// broken manifest.
#[test]
fn selected_toml_matches_fixture_dirs() {
    let listed: BTreeSet<String> = selected_cases().into_iter().collect();
    let on_disk: BTreeSet<String> = std::fs::read_dir(fixtures_root())
        .expect("read fixtures dir")
        .map(|e| e.expect("dir entry"))
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        listed, on_disk,
        "harness/selected.toml's [sparql_default_graph].tests has drifted from \
         crates/harness/tests/fixtures/sparql11/default_graph_subset/"
    );
}
