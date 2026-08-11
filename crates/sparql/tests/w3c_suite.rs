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
use horndb_sparql::exec::{FullBackend, StoreTestExt};
use horndb_sparql::results::json::{write_ask_json, write_select_json};
use horndb_sparql::{DefaultGraphMode, SparqlConfig};
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/selected_subset");
    p
}

fn load_ntriples<S: StoreTestExt + Default>(path: &Path) -> S {
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

// ── MemStore leg (original; keep this name so CI references stay valid) ──────

macro_rules! w3c_case {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<MemStore>($dir);
        }
    };
}

w3c_case!(basic_001, "basic-001");
w3c_case!(basic_002, "basic-002");
w3c_case!(basic_003, "basic-003");
w3c_case!(basic_004, "basic-004");
w3c_case!(basic_005, "basic-005");
w3c_case!(expr_001, "expr-001");
w3c_case!(expr_002, "expr-002");
// Non-recursive property paths (SPEC-07 #49): alternative `|`, negated
// property set `!`, zero-or-one `?`.
w3c_case!(path_alt_001, "path-alt-001");
w3c_case!(path_neg_001, "path-neg-001");
w3c_case!(path_opt_001, "path-opt-001");
// Recursive Kleene property paths (SPEC-07 #50): `+` transitive, `*`
// reflexive-transitive (`subClassOf*`, acceptance #7 shape).
w3c_case!(path_plus_001, "path-plus-001");
w3c_case!(path_star_001, "path-star-001");

// W3C SPARQL 1.0 `graph/` family (SPEC-28 S7). Mirrored from
// <https://w3c.github.io/rdf-tests/sparql/sparql10/graph/>; each case's
// dataset is the manifest's `qt:data` (the default graph) + `qt:graphData`
// (the named graphs), so these run in `strict` mode — see `read_mode`.
// The 3 upstream cases left out are in `harness/KNOWN-MANIFEST-BUGS.md`.
w3c_case!(graph_01, "graph-01");
w3c_case!(graph_02, "graph-02");
w3c_case!(graph_03, "graph-03");
w3c_case!(graph_04, "graph-04");
w3c_case!(graph_05, "graph-05");
w3c_case!(graph_06, "graph-06");
w3c_case!(graph_07, "graph-07");
w3c_case!(graph_08, "graph-08");
w3c_case!(graph_09, "graph-09");
w3c_case!(graph_10b, "graph-10b");
w3c_case!(graph_empty, "graph-empty");
w3c_case!(graph_exist, "graph-exist");
w3c_case!(graph_not_exist, "graph-not-exist");
w3c_case!(graph_variable_join, "graph-variable-join");

// W3C SPARQL 1.0 `dataset/` family (SPEC-28 S7). Mirrored from
// <https://w3c.github.io/rdf-tests/sparql/sparql10/dataset/>; each query
// carries its own `FROM` / `FROM NAMED`, which fixes the dataset
// regardless of the `default_graph` mode. The 2 upstream cases left out
// are in `harness/KNOWN-MANIFEST-BUGS.md`.
w3c_case!(dataset_01, "dataset-01");
w3c_case!(dataset_02, "dataset-02");
w3c_case!(dataset_03, "dataset-03");
w3c_case!(dataset_04, "dataset-04");
w3c_case!(dataset_05, "dataset-05");
w3c_case!(dataset_06, "dataset-06");
w3c_case!(dataset_07, "dataset-07");
w3c_case!(dataset_08, "dataset-08");
w3c_case!(dataset_09b, "dataset-09b");
w3c_case!(dataset_10b, "dataset-10b");

// ── HornBackend leg ───────────────────────────────────────────────────────────

macro_rules! w3c_case_horn {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<HornBackend>($dir);
        }
    };
}

w3c_case_horn!(basic_001_hornbackend, "basic-001");
w3c_case_horn!(basic_002_hornbackend, "basic-002");
w3c_case_horn!(basic_003_hornbackend, "basic-003");
w3c_case_horn!(basic_004_hornbackend, "basic-004");
w3c_case_horn!(basic_005_hornbackend, "basic-005");
w3c_case_horn!(expr_001_hornbackend, "expr-001");
w3c_case_horn!(expr_002_hornbackend, "expr-002");
w3c_case_horn!(path_alt_001_hornbackend, "path-alt-001");
w3c_case_horn!(path_neg_001_hornbackend, "path-neg-001");
w3c_case_horn!(path_opt_001_hornbackend, "path-opt-001");
w3c_case_horn!(path_plus_001_hornbackend, "path-plus-001");
w3c_case_horn!(path_star_001_hornbackend, "path-star-001");

// W3C SPARQL 1.0 `graph/` family (SPEC-28 S7). Mirrored from
// <https://w3c.github.io/rdf-tests/sparql/sparql10/graph/>; each case's
// dataset is the manifest's `qt:data` (the default graph) + `qt:graphData`
// (the named graphs), so these run in `strict` mode — see `read_mode`.
// The 3 upstream cases left out are in `harness/KNOWN-MANIFEST-BUGS.md`.
w3c_case_horn!(graph_01_hornbackend, "graph-01");
w3c_case_horn!(graph_02_hornbackend, "graph-02");
w3c_case_horn!(graph_03_hornbackend, "graph-03");
w3c_case_horn!(graph_04_hornbackend, "graph-04");
w3c_case_horn!(graph_05_hornbackend, "graph-05");
w3c_case_horn!(graph_06_hornbackend, "graph-06");
w3c_case_horn!(graph_07_hornbackend, "graph-07");
w3c_case_horn!(graph_08_hornbackend, "graph-08");
w3c_case_horn!(graph_09_hornbackend, "graph-09");
w3c_case_horn!(graph_10b_hornbackend, "graph-10b");
w3c_case_horn!(graph_empty_hornbackend, "graph-empty");
w3c_case_horn!(graph_exist_hornbackend, "graph-exist");
w3c_case_horn!(graph_not_exist_hornbackend, "graph-not-exist");
w3c_case_horn!(graph_variable_join_hornbackend, "graph-variable-join");

// W3C SPARQL 1.0 `dataset/` family (SPEC-28 S7). Mirrored from
// <https://w3c.github.io/rdf-tests/sparql/sparql10/dataset/>; each query
// carries its own `FROM` / `FROM NAMED`, which fixes the dataset
// regardless of the `default_graph` mode. The 2 upstream cases left out
// are in `harness/KNOWN-MANIFEST-BUGS.md`.
w3c_case_horn!(dataset_01_hornbackend, "dataset-01");
w3c_case_horn!(dataset_02_hornbackend, "dataset-02");
w3c_case_horn!(dataset_03_hornbackend, "dataset-03");
w3c_case_horn!(dataset_04_hornbackend, "dataset-04");
w3c_case_horn!(dataset_05_hornbackend, "dataset-05");
w3c_case_horn!(dataset_06_hornbackend, "dataset-06");
w3c_case_horn!(dataset_07_hornbackend, "dataset-07");
w3c_case_horn!(dataset_08_hornbackend, "dataset-08");
w3c_case_horn!(dataset_09b_hornbackend, "dataset-09b");
w3c_case_horn!(dataset_10b_hornbackend, "dataset-10b");
