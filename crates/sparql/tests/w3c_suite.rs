//! Drives the Stage-1 W3C SPARQL Query subset committed in
//! `crates/harness/tests/fixtures/sparql11/`. Diffs each query's
//! answer against the vendored expected SPARQL-JSON file.

use reasoner_sparql::algebra::Term;
use reasoner_sparql::api::{execute_query, QueryAnswer};
use reasoner_sparql::exec::mem::MemStore;
use reasoner_sparql::exec::Store;
use reasoner_sparql::results::json::{write_ask_json, write_select_json};
use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/selected_subset");
    p
}

fn load_ntriples(path: &PathBuf) -> MemStore {
    let mut s = MemStore::default();
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
    } else if input.starts_with('"') {
        // find the closing quote (no escape handling — fixtures are simple).
        let rest = &input[1..];
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

fn read_form(dir: &PathBuf) -> String {
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

fn run_one(name: &str) {
    let dir = fixtures_root().join(name);
    let store = load_ntriples(&dir.join("data.nt"));
    let q = std::fs::read_to_string(dir.join("query.rq")).expect("read query.rq");
    let expected = std::fs::read_to_string(dir.join("expected.srj")).expect("read expected.srj");
    let form = read_form(&dir);

    let ans = execute_query(&q, &store).unwrap_or_else(|e| panic!("{name}: {e}"));
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

macro_rules! w3c_case {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one($dir);
        }
    };
}

w3c_case!(basic_001, "basic-001");
w3c_case!(basic_002, "basic-002");
w3c_case!(basic_003, "basic-003");
w3c_case!(basic_004, "basic-004");
w3c_case!(basic_005, "basic-005");
