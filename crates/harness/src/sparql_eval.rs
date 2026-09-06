//! Grading for the W3C SPARQL 1.1 **evaluation** suite (`sparql11-eval`).
//!
//! The curated `[sparql_query]` / `[sparql_update]` sections grade *mirrored*
//! fixture directories. This module instead grades the upstream manifest tree
//! as-fetched, so the whole suite can be selected without hand-mirroring ~370
//! cases. It runs the SPEC-07 engine directly (`horndb-sparql`) rather than
//! through the `Reasoner` trait — the same way the syntax suite calls
//! `spargebra` directly — because result-set evaluation is not a reasoning
//! question.
//!
//! Conventions, all of them the ones the upstream manifests assume:
//!
//! * Every file IRI is the local `file://<path>`. A query's relative IRIs
//!   (`GRAPH <exists02.ttl>`) must resolve against the query file, so each
//!   query/update text gets one `BASE <file://…>` line prepended. That makes
//!   the resolved graph name equal the `file://` IRI a `qt:graphData` file is
//!   loaded under, which is what those cases compare against.
//! * `qt:data` is the default graph and `qt:graphData` the named graphs, so
//!   queries run in [`DefaultGraphMode::Strict`] — under `Union` the named
//!   graphs would leak into the default graph.
//! * The backend is [`HornBackend`], the storage/WCOJ path the server uses.
//!
//! A grading function returns `Ok(None)` for a pass and `Ok(Some(reason))` for
//! a fail. `Err` is reserved for harness faults (unreadable fixture), which the
//! runner surfaces separately so a broken fixture never reads as a test result.

use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use horndb_sparql::algebra::Term as ATerm;
use horndb_sparql::api::{execute_query_with, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::parse_update;
use horndb_sparql::results::json::{write_ask_json, write_select_json};
use horndb_sparql::update::apply_update;
use horndb_sparql::{DefaultGraphMode, SparqlConfig};
use oxrdfio::{RdfFormat, RdfParser};
use serde_json::{json, Value};
use spargebra::algebra::GraphTarget;

/// `Ok(None)` = the case passed; `Ok(Some(reason))` = it failed, with why.
pub(crate) type Verdict = Option<String>;

/// `xsd:string` is the implicit datatype of a plain literal. The two result
/// writers disagree on whether to spell it out, so it is stripped on both
/// sides before comparing.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// Namespace prefix of the XSD numeric datatypes.
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Run one grading closure with panics contained.
///
/// The evaluation suite feeds ~370 arbitrary upstream queries through the
/// engine; a single `unwrap` in a not-yet-supported path would otherwise abort
/// the whole conformance run instead of failing one case. A panic is graded as
/// a failure with its message, so it shows up in the triage like any other red.
pub(crate) fn catch_panic(f: impl FnOnce() -> Result<Verdict>) -> Result<Verdict> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            Ok(Some(format!("engine panicked: {msg}")))
        }
    }
}

fn file_iri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn rdf_format(path: &Path) -> Result<RdfFormat> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ttl") => Ok(RdfFormat::Turtle),
        Some("rdf") | Some("owl") | Some("xml") => Ok(RdfFormat::RdfXml),
        Some("nt") => Ok(RdfFormat::NTriples),
        Some("trig") => Ok(RdfFormat::TriG),
        _ => Err(anyhow!("unknown RDF file extension: {}", path.display())),
    }
}

/// Load one RDF file into `store`, all of it in `graph` (`None` = default
/// graph). Relative IRIs resolve against the file's own `file://` IRI.
fn load_file(store: &mut HornBackend, path: &Path, graph: Option<&str>) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let parser = RdfParser::from_format(rdf_format(path)?).with_base_iri(file_iri(path))?;
    let g = graph.map(|g| oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(g)));
    for quad in parser.for_slice(&bytes) {
        let quad = quad.with_context(|| format!("parsing {}", path.display()))?;
        let s = oxrdf::Term::from(quad.subject);
        let p = oxrdf::Term::from(quad.predicate);
        match &g {
            None => store.insert_oxrdf(&s, &p, &quad.object)?,
            Some(g) => store.insert_oxrdf_in_named_graph(g, &s, &p, &quad.object)?,
        };
    }
    // SPEC-25 S5 acceptance #5: under `HORNDB_COLD_TIER=1` the whole store
    // goes cold once the file is loaded, so every case below queries a cold
    // store. Here rather than in `insert_oxrdf` because each demote encodes a
    // whole partition — per triple it would be quadratic.
    store.demote_all_if_cold_tier();
    Ok(())
}

/// Prepend the `BASE` the upstream case assumes: the query/update file's own
/// IRI. SPARQL allows `BASE` anywhere in the prologue and a later `BASE` in the
/// text still wins, so this never changes a query that declares its own.
fn with_base(text: &str, path: &Path) -> String {
    format!("BASE <{}>\n{text}", file_iri(path))
}

// ── Query evaluation (`mf:QueryEvaluationTest`) ──────────────────────────────

pub(crate) fn run_query_eval(
    query: &Path,
    data: Option<&Path>,
    graph_data: &[std::path::PathBuf],
    result: &Path,
) -> Result<Verdict> {
    let mut store = HornBackend::new();
    if let Some(d) = data {
        load_file(&mut store, d, None)?;
    }
    for g in graph_data {
        let name = file_iri(g);
        load_file(&mut store, g, Some(&name))?;
    }

    let expected = match read_expected(result)? {
        Some(e) => e,
        None => {
            return Ok(Some(format!(
                "result format not graded yet: {}",
                result.display()
            )))
        }
    };

    let text = std::fs::read_to_string(query)
        .with_context(|| format!("reading query {}", query.display()))?;
    let cfg = SparqlConfig {
        default_graph: DefaultGraphMode::Strict,
        ..SparqlConfig::default()
    };
    let answer = match execute_query_with(&with_base(&text, query), &store, &cfg) {
        Ok(a) => a,
        Err(e) => return Ok(Some(format!("query failed: {e}"))),
    };

    Ok(match (answer, expected) {
        (QueryAnswer::Boolean(got), Expected::Boolean(want)) => {
            if got == want {
                None
            } else {
                Some(format!("ASK got {got}, expected {want}"))
            }
        }
        (QueryAnswer::Solutions { vars, rows }, Expected::Solutions { vars: ev, rows: er }) => {
            compare_solutions(&write_select_json(&vars, &rows), &ev, &er)
        }
        (QueryAnswer::Boolean(got), Expected::Solutions { .. }) => Some(format!(
            "expected a result set, engine answered ASK {}",
            write_ask_json(got)
        )),
        (QueryAnswer::Solutions { .. }, Expected::Boolean(_)) => {
            Some("expected a boolean, engine answered a result set".into())
        }
        (other, _) => Some(format!("unsupported answer shape: {other:?}")),
    })
}

/// The expected answer, in the same SPARQL-JSON shape the engine emits.
enum Expected {
    Boolean(bool),
    Solutions { vars: Vec<String>, rows: Vec<Value> },
}

/// Read an `mf:result` file. `Ok(None)` means the format is one this runner
/// does not grade yet (`.ttl` CONSTRUCT graphs, `.csv`/`.tsv` serializations)
/// — the caller turns that into a visible failure rather than a silent pass.
fn read_expected(path: &Path) -> Result<Option<Expected>> {
    use sparesults::{QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput};
    let fmt = match path.extension().and_then(|e| e.to_str()) {
        Some("srx") => QueryResultsFormat::Xml,
        Some("srj") => QueryResultsFormat::Json,
        _ => return Ok(None),
    };
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let out = QueryResultsParser::from_format(fmt)
        .for_reader(bytes.as_slice())
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(match out {
        ReaderQueryResultsParserOutput::Boolean(b) => Expected::Boolean(b),
        ReaderQueryResultsParserOutput::Solutions(solutions) => {
            let vars = solutions
                .variables()
                .iter()
                .map(|v| v.as_str().to_string())
                .collect();
            let mut rows = Vec::new();
            for s in solutions {
                let s = s.with_context(|| format!("reading solution from {}", path.display()))?;
                let mut obj = serde_json::Map::new();
                for (var, term) in s.iter() {
                    obj.insert(var.as_str().to_string(), oxterm_to_json(term));
                }
                rows.push(Value::Object(obj));
            }
            Expected::Solutions { vars, rows }
        }
    }))
}

fn oxterm_to_json(t: &oxrdf::Term) -> Value {
    match t {
        oxrdf::Term::NamedNode(n) => json!({ "type": "uri", "value": n.as_str() }),
        oxrdf::Term::BlankNode(b) => json!({ "type": "bnode", "value": b.as_str() }),
        oxrdf::Term::Literal(l) => match l.language() {
            Some(lang) => json!({ "type": "literal", "value": l.value(), "xml:lang": lang }),
            None => {
                json!({ "type": "literal", "value": l.value(), "datatype": l.datatype().as_str() })
            }
        },
        other => json!({ "type": "literal", "value": other.to_string() }),
    }
}

/// One canonical spelling per numeric value, so two lexical forms of the same
/// number compare equal within their datatype.
///
/// The upstream `.srx` files were written by several engines over a decade and
/// spell the same value more than one way: `"3"` and `"3.0"` for the
/// `xsd:decimal` 3, `"2E-1"` and `"2.0E-1"` for the `xsd:double` 0.2. Two
/// cases in the same suite even disagree with each other (`functions/ceil01`
/// wants `"3"`, `aggregates/agg-avg-02` wants `"2.0"`), so no single canonical
/// output form can satisfy both — the comparison has to be by value.
///
/// The *datatype* is deliberately kept: `xsd:integer` 3 and `xsd:decimal` 3
/// must still differ, since which one an expression returns is exactly what
/// the numeric-typing cases test.
fn canonical_numeric(datatype: &str, value: &str) -> Option<String> {
    let local = datatype.strip_prefix(XSD)?;
    let v = value.trim();
    Some(match local {
        "integer" | "long" | "int" | "short" | "byte" | "nonNegativeInteger"
        | "nonPositiveInteger" | "negativeInteger" | "positiveInteger" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => v.parse::<i128>().ok()?.to_string(),
        // Fixed point, not f64: 11.1 must not become 11.100000000000001.
        "decimal" => oxsdatatypes::Decimal::from_str(v).ok()?.to_string(),
        // `INF`/`-INF`/`NaN` do not parse as f64; they are already canonical,
        // so leaving them untouched compares them verbatim.
        "float" | "double" => format!("{:E}", v.parse::<f64>().ok()?),
        _ => return None,
    })
}

/// Drop an explicit `xsd:string` datatype so the two writers' spellings of a
/// plain literal compare equal, and put numeric literals in one canonical
/// spelling per value (see [`canonical_numeric`]).
fn normalize(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        if obj.get("datatype").and_then(Value::as_str) == Some(XSD_STRING) {
            obj.remove("datatype");
        }
        let canonical = match (obj.get("datatype"), obj.get("value")) {
            (Some(Value::String(dt)), Some(Value::String(value))) => canonical_numeric(dt, value),
            _ => None,
        };
        if let Some(c) = canonical {
            obj.insert("value".to_owned(), Value::String(c));
        }
    }
    v
}

/// Compare the engine's SPARQL-JSON answer against the expected variables and
/// rows: variables as a set, rows as a multiset (SPARQL result sets are
/// unordered unless the query says `ORDER BY`, and this runner does not yet
/// grade `mf:ResultOrdering`).
fn compare_solutions(got_json: &str, want_vars: &[String], want_rows: &[Value]) -> Verdict {
    let g: Value = serde_json::from_str(got_json).expect("engine emits valid JSON");
    let gv: HashSet<&str> = g["head"]["vars"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let wv: HashSet<&str> = want_vars.iter().map(String::as_str).collect();
    if gv != wv {
        return Some(format!("vars differ: got {gv:?}, expected {wv:?}"));
    }
    let key = |rows: &[Value]| -> Vec<String> {
        let mut v: Vec<String> = rows
            .iter()
            .map(|r| {
                let obj: serde_json::Map<String, Value> = r
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(k, val)| (k, normalize(val)))
                    .collect();
                Value::Object(obj).to_string()
            })
            .collect();
        v.sort();
        v
    };
    let got_rows: Vec<Value> = g["results"]["bindings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (gk, wk) = (key(&got_rows), key(want_rows));
    if gk == wk {
        return None;
    }
    let only_got: Vec<&String> = gk.iter().filter(|r| !wk.contains(r)).collect();
    let only_want: Vec<&String> = wk.iter().filter(|r| !gk.contains(r)).collect();
    Some(format!(
        "{} rows vs {} expected; only in answer: {only_got:?}; only in expected: {only_want:?}",
        gk.len(),
        wk.len()
    ))
}

// ── Update evaluation (`mf:UpdateEvaluationTest`) ────────────────────────────

pub(crate) fn run_update_eval(
    request: &Path,
    data: Option<&Path>,
    graph_data: &[(std::path::PathBuf, String)],
    result_data: Option<&Path>,
    result_graph_data: &[(std::path::PathBuf, String)],
) -> Result<Verdict> {
    let mut store = seed(data, graph_data)?;
    let text = std::fs::read_to_string(request)
        .with_context(|| format!("reading update {}", request.display()))?;
    let parsed = match parse_update(&with_base(&text, request)) {
        Ok(u) => u,
        Err(e) => return Ok(Some(format!("update parse failed: {e}"))),
    };
    if let Err(e) = apply_update(&parsed, &mut store) {
        return Ok(Some(format!("update failed: {e}")));
    }
    let expected = seed(result_data, result_graph_data)?;

    let got = dump(&store);
    let want = dump(&expected);
    if got == want {
        return Ok(None);
    }
    let only_got: Vec<_> = got.difference(&want).take(5).collect();
    let only_want: Vec<_> = want.difference(&got).take(5).collect();
    Ok(Some(format!(
        "final state differs ({} vs {} quads); only in store: {only_got:?}; only in expected: {only_want:?}",
        got.len(),
        want.len()
    )))
}

fn seed(data: Option<&Path>, graph_data: &[(std::path::PathBuf, String)]) -> Result<HornBackend> {
    let mut store = HornBackend::new();
    if let Some(d) = data {
        load_file(&mut store, d, None)?;
    }
    for (file, name) in graph_data {
        load_file(&mut store, file, Some(name))?;
    }
    Ok(store)
}

/// Every visible quad, keyed by graph (`None` = default graph). SPEC-28 D11:
/// a named graph exists iff it holds at least one quad, so an
/// empty-but-existing graph is indistinguishable from an absent one.
fn dump(store: &HornBackend) -> HashSet<(Option<String>, ATerm, ATerm, ATerm)> {
    let mut out = HashSet::new();
    if let Ok(triples) = store.scan_graph_quads(&GraphTarget::DefaultGraph) {
        for (s, p, o) in triples {
            out.insert((None, s, p, o));
        }
    }
    for g in store.graphs() {
        let target = GraphTarget::NamedNode(spargebra::term::NamedNode::new_unchecked(&g));
        if let Ok(triples) = store.scan_graph_quads(&target) {
            for (s, p, o) in triples {
                out.insert((Some(g.clone()), s, p, o));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_literals_compare_by_value_within_their_datatype() {
        let lit = |dt: &str, v: &str| json!({ "type": "literal", "value": v, "datatype": format!("{XSD}{dt}") });
        // Same value, different spelling → equal.
        assert_eq!(
            normalize(lit("decimal", "3")),
            normalize(lit("decimal", "3.0"))
        );
        assert_eq!(
            normalize(lit("double", "2E-1")),
            normalize(lit("double", "2.0E-1"))
        );
        // Different datatype, or different value → still different.
        assert_ne!(
            normalize(lit("integer", "3")),
            normalize(lit("decimal", "3.0"))
        );
        assert_ne!(
            normalize(lit("decimal", "3")),
            normalize(lit("decimal", "3.5"))
        );
        // Exact decimals: an f64 round trip would collapse these two.
        assert_ne!(
            normalize(lit("decimal", "11.1")),
            normalize(lit("decimal", "11.100000000000001"))
        );
    }

    #[test]
    fn xsd_string_datatype_is_normalized_away() {
        let plain = json!({ "type": "literal", "value": "x" });
        let spelled = json!({ "type": "literal", "value": "x", "datatype": XSD_STRING });
        assert_eq!(normalize(spelled), plain);
    }

    #[test]
    fn compare_solutions_is_order_insensitive_and_reports_diffs() {
        let got = r#"{"head":{"vars":["s"]},"results":{"bindings":[
            {"s":{"type":"uri","value":"http://b"}},
            {"s":{"type":"uri","value":"http://a"}}]}}"#;
        let want = vec![
            json!({ "s": { "type": "uri", "value": "http://a" } }),
            json!({ "s": { "type": "uri", "value": "http://b" } }),
        ];
        assert_eq!(compare_solutions(got, &["s".into()], &want), None);
        assert!(compare_solutions(got, &["s".into()], &want[..1])
            .expect("row count differs")
            .contains("only in answer"));
        assert!(compare_solutions(got, &["t".into()], &want)
            .expect("vars differ")
            .contains("vars differ"));
    }
}
