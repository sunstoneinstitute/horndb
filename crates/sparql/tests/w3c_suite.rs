//! Drives the Stage-1 W3C SPARQL Query subset committed in
//! `crates/harness/tests/fixtures/sparql11/`. Diffs each query's
//! answer against the vendored expected SPARQL-JSON file. The case list
//! mirrors `harness/selected.toml`'s `[sparql_query]` section.
//!
//! Two backends are exercised:
//! * [`MemStore`] — the original Stage-1 hash-set backend.
//! * [`HornBackend`] — the storage/WCOJ backend wired in by issue #67.
//!
//! A case directory carries `query.rq`, `form`, `expected.srj`, and its
//! data as either `data.nt` (default graph only) or `data.trig` (named
//! graphs — the W3C `graph/` + `dataset/` families, SPEC-28 S7). An
//! optional `default-graph` file picks the `default_graph` mode.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query_with, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, Store};
use horndb_sparql::results::json::{write_ask_json, write_select_json};
use horndb_sparql::{DefaultGraphMode, SparqlConfig};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/selected_subset");
    p
}

fn load_ntriples<S: Store + Default>(path: &Path) -> S {
    let mut s = S::default();
    let body = std::fs::read_to_string(path).expect("read data.nt");
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Minimal N-Triples line parser: <s> <p> <o> . OR
        // <s> <p> "lit" .
        let line = line.trim_end_matches('.').trim();
        let (subj, rest) = split_term(line);
        let (pred, rest) = split_term(rest.trim());
        let obj = rest.trim().trim_end_matches('.').trim().to_owned();
        s.insert_triple(parse_term(&subj), parse_term(&pred), parse_term(&obj));
    }
    s
}

fn split_term(input: &str) -> (String, &str) {
    let input = input.trim_start();
    if input.starts_with('<') {
        let end = input.find('>').unwrap();
        (input[..=end].to_owned(), &input[end + 1..])
    } else if let Some(rest) = input.strip_prefix('"') {
        // find the closing quote (no escape handling — fixtures are simple).
        let end = rest.find('"').unwrap();
        (input[..=end + 1].to_owned(), &input[end + 2..])
    } else {
        // bnode `_:foo`
        let end = input.find(char::is_whitespace).unwrap();
        (input[..end].to_owned(), &input[end..])
    }
}

fn parse_term(s: &str) -> Term {
    if let Some(inner) = s.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        Term::Iri(inner.to_owned())
    } else if s.starts_with('"') {
        Term::Literal(s.to_owned())
    } else if let Some(rest) = s.strip_prefix("_:") {
        Term::BlankNode(rest.to_owned())
    } else {
        Term::Literal(s.to_owned())
    }
}

// ── Named-graph inputs: `data.trig` (SPEC-28 S7) ─────────────────────────────

/// Seed one quad into a backend. `None` is the default graph.
///
/// The write trait (`exec::Store`) is still triple-shaped and default-graph
/// only — the named-graph write path is SPEC-28 phase 4 (#267) — so a
/// `data.trig` case plants its quads through each backend's storage seam,
/// the same way `tests/graph_query.rs` does.
trait QuadSeed {
    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad);
}

impl QuadSeed for MemStore {
    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad) {
        // `MemStore` keeps terms as their N-Triples lexical form, with IRIs
        // and blank-node labels *bare* (`term_to_lex` in `exec/mem.rs`).
        self.insert_quad(
            graph.map(oxrdf::NamedNode::as_str),
            (
                lex(&q.subject.clone().into()),
                lex(&q.predicate.clone().into()),
                lex(&q.object),
            ),
        );
    }
}

impl QuadSeed for HornBackend {
    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad) {
        let s = oxrdf::Term::from(q.subject.clone());
        let p = oxrdf::Term::from(q.predicate.clone());
        match graph {
            None => {
                self.insert_oxrdf(&s, &p, &q.object).unwrap();
            }
            Some(g) => {
                let g = oxrdf::Term::from(g.clone());
                self.insert_oxrdf_in_named_graph(&g, &s, &p, &q.object)
                    .unwrap();
            }
        }
    }
}

/// `MemStore`'s lexical form of an oxrdf term: bare IRI / bare blank-node
/// label / N-Triples literal.
fn lex(t: &oxrdf::Term) -> String {
    match t {
        oxrdf::Term::NamedNode(n) => n.as_str().to_owned(),
        oxrdf::Term::BlankNode(b) => b.as_str().to_owned(),
        other => other.to_string(),
    }
}

fn load_trig<S: QuadSeed + Default>(path: &Path) -> S {
    let mut s = S::default();
    let bytes = std::fs::read(path).expect("read data.trig");
    for q in oxttl::TriGParser::new().for_slice(&bytes) {
        let q = q.expect("parse data.trig");
        let graph = match &q.graph_name {
            oxrdf::GraphName::DefaultGraph => None,
            oxrdf::GraphName::NamedNode(g) => Some(g.clone()),
            oxrdf::GraphName::BlankNode(_) => panic!("blank-node graph names are not used"),
        };
        s.seed_quad(graph.as_ref(), &q);
    }
    s
}

fn read_form(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("form"))
        .expect("read form")
        .trim()
        .to_owned()
}

fn assert_select_equal(got: &str, expected: &str) {
    let g: serde_json::Value = serde_json::from_str(got).unwrap();
    let e: serde_json::Value = serde_json::from_str(expected).unwrap();
    // vars: compare as set
    let gv: std::collections::BTreeSet<String> = g["head"]["vars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let ev: std::collections::BTreeSet<String> = e["head"]["vars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(gv, ev, "vars differ");
    // bindings: compare as multiset (sort by serialised form)
    let mut gb: Vec<String> = g["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| serde_json::to_string(b).unwrap())
        .collect();
    let mut eb: Vec<String> = e["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| serde_json::to_string(b).unwrap())
        .collect();
    gb.sort();
    eb.sort();
    assert_eq!(gb, eb, "bindings differ");
}

/// The case's `default_graph` mode, from an optional `default-graph` file.
/// Absent means `union` — the crate default — and writing `union` there
/// explicitly means the same thing.
///
/// The W3C `graph/` family fixes its dataset in the *manifest*
/// (`qt:data` = the default graph, `qt:graphData` = the named graphs) rather
/// than in the query, so its mirrored cases run in `strict` mode — that is
/// the mode whose default graph is exactly `qt:data` (SPEC-28 D2). The
/// `dataset/` family's queries carry their own `FROM`/`FROM NAMED`, which
/// wins over the mode, so those run under the default.
fn read_mode(dir: &Path) -> DefaultGraphMode {
    match std::fs::read_to_string(dir.join("default-graph")) {
        Err(_) => DefaultGraphMode::Union,
        Ok(s) => match s.trim() {
            "strict" => DefaultGraphMode::Strict,
            "union" => DefaultGraphMode::Union,
            other => panic!("unknown default-graph value {other:?} in {}", dir.display()),
        },
    }
}

fn run_one<B: FullBackend + QuadSeed + Default>(name: &str) {
    let dir = fixtures_root().join(name);
    // A case carries either `data.nt` (default graph only) or `data.trig`
    // (named graphs — SPEC-28 S7).
    let trig = dir.join("data.trig");
    let backend: B = if trig.exists() {
        load_trig(&trig)
    } else {
        load_ntriples(&dir.join("data.nt"))
    };
    let q = std::fs::read_to_string(dir.join("query.rq")).expect("read query.rq");
    let expected = std::fs::read_to_string(dir.join("expected.srj")).expect("read expected.srj");
    let form = read_form(&dir);
    let cfg = SparqlConfig {
        default_graph: read_mode(&dir),
        ..SparqlConfig::default()
    };

    let ans = execute_query_with(&q, &backend, &cfg).unwrap_or_else(|e| panic!("{name}: {e}"));
    match (form.as_str(), ans) {
        ("select", QueryAnswer::Solutions { vars, rows }) => {
            let got = write_select_json(&vars, &rows);
            assert_select_equal(&got, &expected);
        }
        ("ask", QueryAnswer::Boolean(b)) => {
            let got = write_ask_json(b);
            let g: serde_json::Value = serde_json::from_str(&got).unwrap();
            let e: serde_json::Value = serde_json::from_str(&expected).unwrap();
            assert_eq!(g["boolean"], e["boolean"], "{name}: boolean differs");
        }
        (form, ans) => panic!("{name}: unexpected form/answer pair {form:?} / {ans:?}"),
    }
}

// ── Case list ─────────────────────────────────────────────────────────────────
//
// One list drives both backends (HDB-76): each `name => "dir"` entry expands
// to a `#[test] fn name()` running `MemStore` here and an identically-named
// `#[test] fn name()` running `HornBackend` inside `mod horn` below, plus a
// `CASE_DIRS` entry. Previously this list was hand-duplicated between a
// `w3c_case!` and a `w3c_case_horn!` macro invocation block; a case added to
// one and not the other (or added to `harness/selected.toml`'s
// `[sparql_query]` and not here) would silently never run. The
// `w3c_case_list_matches_selected_toml` test below asserts `CASE_DIRS`
// exactly matches that manifest section, so drift now fails the build
// instead of quietly shrinking the gate.
macro_rules! w3c_case {
    ($($name:ident => $dir:expr),+ $(,)?) => {
        $(
            #[test]
            fn $name() {
                run_one::<MemStore>($dir);
            }
        )+

        mod horn {
            use super::*;

            $(
                #[test]
                fn $name() {
                    run_one::<HornBackend>($dir);
                }
            )+
        }

        /// Case directory names this invocation drives, on both backends —
        /// read by `w3c_case_list_matches_selected_toml` to catch drift
        /// against `harness/selected.toml`'s `[sparql_query].tests`.
        const CASE_DIRS: &[&str] = &[$($dir),+];
    };
}

w3c_case! {
    basic_001 => "basic-001",
    basic_002 => "basic-002",
    basic_003 => "basic-003",
    basic_004 => "basic-004",
    basic_005 => "basic-005",
    expr_001 => "expr-001",
    expr_002 => "expr-002",
    // Non-recursive property paths (SPEC-07 #49): alternative `|`, negated
    // property set `!`, zero-or-one `?`.
    path_alt_001 => "path-alt-001",
    path_neg_001 => "path-neg-001",
    path_opt_001 => "path-opt-001",
    // Recursive Kleene property paths (SPEC-07 #50): `+` transitive, `*`
    // reflexive-transitive (`subClassOf*`, acceptance #7 shape).
    path_plus_001 => "path-plus-001",
    path_star_001 => "path-star-001",

    // W3C SPARQL 1.0 `graph/` family (SPEC-28 S7). Mirrored from
    // <https://w3c.github.io/rdf-tests/sparql/sparql10/graph/>; each case's
    // dataset is the manifest's `qt:data` (the default graph) + `qt:graphData`
    // (the named graphs), so these run in `strict` mode — see `read_mode`.
    // The one upstream case left out (`graph-11`) is in
    // `harness/KNOWN-MANIFEST-BUGS.md`.
    graph_01 => "graph-01",
    graph_02 => "graph-02",
    graph_03 => "graph-03",
    graph_04 => "graph-04",
    graph_05 => "graph-05",
    graph_06 => "graph-06",
    graph_07 => "graph-07",
    graph_08 => "graph-08",
    graph_09 => "graph-09",
    graph_10b => "graph-10b",
    graph_empty => "graph-empty",
    graph_exist => "graph-exist",
    graph_not_exist => "graph-not-exist",
    graph_optional => "graph-optional",
    graph_variable_join => "graph-variable-join",
    graph_variable_scope => "graph-variable-scope",

    // W3C SPARQL 1.0 `dataset/` family (SPEC-28 S7). Mirrored from
    // <https://w3c.github.io/rdf-tests/sparql/sparql10/dataset/>; each query
    // carries its own `FROM` / `FROM NAMED`, which fixes the dataset
    // regardless of the `default_graph` mode. The 2 upstream cases left out
    // are in `harness/KNOWN-MANIFEST-BUGS.md`.
    dataset_01 => "dataset-01",
    dataset_02 => "dataset-02",
    dataset_03 => "dataset-03",
    dataset_04 => "dataset-04",
    dataset_05 => "dataset-05",
    dataset_06 => "dataset-06",
    dataset_07 => "dataset-07",
    dataset_08 => "dataset-08",
    dataset_09b => "dataset-09b",
    dataset_10b => "dataset-10b",
}

// ── Manifest drift guard (HDB-76) ────────────────────────────────────────────

/// The slice of `harness/selected.toml` this guard reads: just enough of
/// the schema (see `horndb_harness::selected::Selected`, the full loader) to
/// pull out `[sparql_query].tests`. A local subset rather than a dependency
/// on `horndb-harness` — that crate sits on top of `horndb-sparql` in the
/// workspace's dependency order, so depending on it from here would invert
/// that layering.
#[derive(serde::Deserialize)]
struct SelectedManifest {
    sparql_query: SparqlQuerySection,
}

#[derive(serde::Deserialize)]
struct SparqlQuerySection {
    tests: Vec<String>,
}

/// Asserts `CASE_DIRS` (the single case list above, driving both backends)
/// is exactly `harness/selected.toml`'s `[sparql_query].tests`, so a case
/// added to one and not the other fails the build instead of silently
/// shrinking the gate.
#[test]
fn w3c_case_list_matches_selected_toml() {
    // CARGO_MANIFEST_DIR is crates/sparql; selected.toml lives at the
    // workspace root, two levels up.
    let mut manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_path.pop(); // crates/
    manifest_path.pop(); // workspace root
    manifest_path.push("harness/selected.toml");

    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
    let doc: SelectedManifest =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", manifest_path.display()));

    let manifest_cases: BTreeSet<String> = doc
        .sparql_query
        .tests
        .iter()
        .map(|t| {
            t.strip_prefix("selected_subset/")
                .unwrap_or_else(|| {
                    panic!("[sparql_query].tests entry {t:?} not under selected_subset/")
                })
                .to_owned()
        })
        .collect();

    let running_cases: BTreeSet<String> = CASE_DIRS.iter().map(|s| s.to_string()).collect();

    let missing_from_run: Vec<&String> = manifest_cases.difference(&running_cases).collect();
    let missing_from_manifest: Vec<&String> = running_cases.difference(&manifest_cases).collect();

    assert!(
        missing_from_run.is_empty() && missing_from_manifest.is_empty(),
        "w3c_suite.rs's case list has drifted from harness/selected.toml's \
         [sparql_query].tests.\n\
         In selected.toml but not run (false green — the gate is smaller than \
         it looks): {missing_from_run:?}\n\
         Run but not in selected.toml (manifest is stale): {missing_from_manifest:?}"
    );
}
