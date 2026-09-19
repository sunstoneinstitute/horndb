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
//!
//! # Expected-result formats
//!
//! Five, each compared at the fidelity its own format carries:
//!
//! * `.srx` / `.srj` — SPARQL Query Results XML / JSON. Full terms, compared
//!   exactly (modulo the `xsd:string` and numeric normalisation below).
//! * `.tsv` — SPARQL Query Results TSV. Also full terms: TSV writes an IRI as
//!   `<iri>` and a literal with its quotes, datatype and language tag, so it is
//!   parsed by `sparesults` into the same shape as `.srx` and graded just as
//!   strictly.
//! * `.csv` — SPARQL Query Results CSV. **Lossy on purpose**; [`csv_cell`]
//!   spells out what that comparison can and cannot catch.
//! * `.ttl` — a CONSTRUCT / DESCRIBE result graph, compared by isomorphism:
//!   both sides go through [`crate::rdf::canonical_graph`], which renames blank
//!   nodes canonically, and are then compared as graphs.
//!
//! Blank-node labels are engine-minted, so **no** comparison here matches them
//! literally: rows are paired under a bijection ([`match_blank_nodes`]) and
//! graphs under canonicalization.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
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
        (QueryAnswer::Solutions { vars, rows }, Expected::Csv { vars: ev, rows: er }) => {
            compare_csv(&write_select_json(&vars, &rows), &ev, &er)
        }
        (QueryAnswer::Triples(got), Expected::Graph(want)) => compare_graph(&got, &want, query),
        (QueryAnswer::Boolean(got), _) => Some(format!(
            "expected a result set, engine answered ASK {}",
            write_ask_json(got)
        )),
        (QueryAnswer::Solutions { .. }, Expected::Boolean(_)) => {
            Some("expected a boolean, engine answered a result set".into())
        }
        (QueryAnswer::Solutions { .. }, Expected::Graph(_)) => {
            Some("expected a graph, engine answered a result set".into())
        }
        (QueryAnswer::Triples(_), _) => {
            Some("expected a result set, engine answered a graph".into())
        }
        (other, _) => Some(format!("unsupported answer shape: {other:?}")),
    })
}

/// The expected answer, in the same SPARQL-JSON shape the engine emits.
enum Expected {
    Boolean(bool),
    Solutions {
        vars: Vec<String>,
        rows: Vec<Value>,
    },
    /// `.csv`: rows already projected into CSV's value space (see [`csv_cell`]).
    Csv {
        vars: Vec<String>,
        rows: Vec<Value>,
    },
    /// `.ttl`: a CONSTRUCT / DESCRIBE result graph, already canonicalized.
    Graph(Box<oxrdf::Graph>),
}

/// Read an `mf:result` file. `Ok(None)` means the format is one this runner
/// does not grade — the caller turns that into a visible failure rather than a
/// silent pass.
fn read_expected(path: &Path) -> Result<Option<Expected>> {
    use sparesults::{QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput};
    let fmt = match path.extension().and_then(|e| e.to_str()) {
        Some("srx") => QueryResultsFormat::Xml,
        Some("srj") => QueryResultsFormat::Json,
        // TSV keeps full term syntax, so `sparesults` can parse it back into
        // real terms. CSV cannot be parsed back (`sparesults` refuses, for the
        // reason spelled out in `csv_cell`), so it gets its own reader.
        Some("tsv") => QueryResultsFormat::Tsv,
        Some("csv") => return read_expected_csv(path).map(Some),
        Some("ttl") | Some("nt") => return read_expected_graph(path).map(Some),
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

// ── CONSTRUCT / DESCRIBE graph results (`.ttl`) ──────────────────────────────

fn read_expected_graph(path: &Path) -> Result<Expected> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let n_triples = path.extension().and_then(|e| e.to_str()) == Some("nt");
    let graph = crate::rdf::canonical_graph(&text, &file_iri(path), n_triples)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Expected::Graph(Box::new(graph)))
}

/// Render one slot of a CONSTRUCT answer as N-Triples.
///
/// [`QueryAnswer::Triples`] carries each term as the lexical string the store
/// holds, with no separate type tag: a literal already arrives in N-Triples
/// form (`"x"`, `"x"@en`, `"x"^^<dt>`), a blank node as `_:label`, and an IRI
/// bare. The leading character is therefore what tells them apart — exactly
/// the N-Triples convention — so only the IRI case needs brackets added.
fn nt_term(s: &str) -> String {
    if s.starts_with('"') || s.starts_with("_:") {
        s.to_owned()
    } else {
        format!("<{s}>")
    }
}

/// Compare a CONSTRUCT answer against the expected graph by isomorphism: equal
/// iff some one-to-one renaming of the answer's blank nodes makes the two
/// triple sets identical. Both sides are canonicalized, so `==` *is* that test.
fn compare_graph(got: &[(String, String, String)], want: &oxrdf::Graph, query: &Path) -> Verdict {
    let mut nt = String::new();
    for (s, p, o) in got {
        nt.push_str(&format!("{} {} {} .\n", nt_term(s), nt_term(p), nt_term(o)));
    }
    let got = match crate::rdf::canonical_graph(&nt, &file_iri(query), true) {
        Ok(g) => g,
        Err(e) => {
            return Some(format!(
                "constructed graph is not well-formed N-Triples: {e}"
            ))
        }
    };
    if &got == want {
        return None;
    }
    Some(format!(
        "constructed graph is not isomorphic to the expected one ({} vs {} triples)",
        got.len(),
        want.len()
    ))
}

// ── CSV results (`.csv`) ─────────────────────────────────────────────────────

/// Project one expected CSV cell into the value space both sides are compared
/// in.
///
/// **What CSV throws away** (SPARQL 1.1 Query Results CSV Format, §"Serializing
/// a Result Set in CSV"): a cell holds an IRI as its bare IRI text and a
/// literal as its bare lexical form, so the datatype, the language tag, and the
/// IRI-vs-literal distinction are all gone; an unbound variable writes the
/// empty string, indistinguishable from an empty literal. Grading a CSV case
/// therefore cannot be term equality — it is equality of this projection.
///
/// So a green CSV case proves the answer has the right shape and the right
/// *values*: right variables, right number of rows, right text in every cell.
/// It does **not** prove the terms are right. These would all pass a CSV case
/// they should fail: returning `"1"^^xsd:string` where `1`(`xsd:integer`) is
/// wanted, `"chat"@fr` where a plain `"chat"` is wanted, the IRI
/// `<http://ex/a>` where the literal `"http://ex/a"` is wanted, and an unbound
/// variable where an empty literal is wanted. The `.srx`/`.srj`/`.tsv` cases,
/// which do carry types, are what catches those.
///
/// A cell spelled `_:label` is read back as a blank node, so blank nodes still
/// pair up by bijection rather than by label. A *literal* whose lexical form
/// happens to start with `_:` is then read as a blank node too — CSV genuinely
/// cannot tell those apart. That direction only ever turns a pass into a
/// failure, never the reverse.
fn csv_cell(text: &str) -> Value {
    match text.strip_prefix("_:") {
        Some(label) => json!({ "type": "bnode", "value": label }),
        None => json!({ "type": "csv", "value": text }),
    }
}

/// Project one row of the engine's SPARQL-JSON answer the same way, over the
/// expected header's variables (CSV has a fixed column per variable).
fn csv_row(row: &Value, vars: &[String]) -> Value {
    let mut obj = serde_json::Map::new();
    for v in vars {
        let cell = match row.get(v) {
            // Kept whole so the bijection can still pair blank nodes.
            Some(t) if t.get("type").and_then(Value::as_str) == Some("bnode") => t.clone(),
            Some(t) => json!({ "type": "csv", "value": t["value"].as_str().unwrap_or_default() }),
            // Unbound. CSV writes the empty string for this and for an empty
            // literal alike, so the projection must too.
            None => json!({ "type": "csv", "value": "" }),
        };
        obj.insert(v.clone(), cell);
    }
    Value::Object(obj)
}

fn read_expected_csv(path: &Path) -> Result<Expected> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut records = read_csv_records(&text).into_iter();
    let vars = records
        .next()
        .ok_or_else(|| anyhow!("{} is empty: no CSV header row", path.display()))?;
    let mut rows = Vec::new();
    for rec in records {
        if rec.len() != vars.len() {
            bail!(
                "{}: CSV row has {} fields, header has {}",
                path.display(),
                rec.len(),
                vars.len()
            );
        }
        let obj: serde_json::Map<String, Value> = vars
            .iter()
            .cloned()
            .zip(rec.iter().map(|c| csv_cell(c)))
            .collect();
        rows.push(Value::Object(obj));
    }
    Ok(Expected::Csv { vars, rows })
}

/// Split RFC 4180 CSV text into records of fields: `""` is an escaped quote
/// inside a quoted field, and a quoted field may contain commas and newlines.
/// Records end at `\n` or `\r\n` (the W3C fixtures use CRLF).
fn read_csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\n' | '\r' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// Grade a `.csv` case: project the engine's answer into CSV's value space and
/// compare it with the expected file, already projected.
fn compare_csv(got_json: &str, want_vars: &[String], want_rows: &[Value]) -> Verdict {
    let g: Value = serde_json::from_str(got_json).expect("engine emits valid JSON");
    let gv: HashSet<&str> = g["head"]["vars"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let wv: HashSet<&str> = want_vars.iter().map(String::as_str).collect();
    if gv != wv {
        return Some(format!("vars differ: got {gv:?}, expected {wv:?}"));
    }
    let got: Vec<Value> = g["results"]["bindings"]
        .as_array()
        .map(|rows| rows.iter().map(|r| csv_row(r, want_vars)).collect())
        .unwrap_or_default();
    compare_rows(&got, want_rows)
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
    let got_rows: Vec<Value> = g["results"]["bindings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    compare_rows(&got_rows, want_rows)
}

/// Compare two row multisets, pairing blank nodes by bijection.
///
/// Two steps. First a multiset comparison with every blank-node label masked:
/// that catches any real difference and gives a readable diff. Then, only if
/// blank nodes are involved, [`match_blank_nodes`] checks that one consistent
/// renaming explains the whole answer — masking alone would accept an answer
/// that shares a blank node between two rows where the expected result does
/// not.
fn compare_rows(got: &[Value], want: &[Value]) -> Verdict {
    let key = |rows: &[Value]| -> Vec<String> {
        let mut v: Vec<String> = rows.iter().map(mask_blank_nodes).collect();
        v.sort();
        v
    };
    let (gk, wk) = (key(got), key(want));
    if gk != wk {
        let only_got: Vec<&String> = gk.iter().filter(|r| !wk.contains(r)).collect();
        let only_want: Vec<&String> = wk.iter().filter(|r| !gk.contains(r)).collect();
        return Some(format!(
            "{} rows vs {} expected; only in answer: {only_got:?}; only in expected: {only_want:?}",
            gk.len(),
            wk.len()
        ));
    }
    if match_blank_nodes(got, want) {
        None
    } else {
        Some(format!(
            "{} rows match cell by cell, but no one-to-one renaming of the answer's blank nodes \
             yields the expected rows (the answer uses {} distinct blank nodes, the expected \
             result {} — so they share nodes between rows differently)",
            gk.len(),
            distinct_blank_nodes(got),
            distinct_blank_nodes(want)
        ))
    }
}

/// One row as a sort key, with every blank-node label replaced by a fixed
/// placeholder — labels are engine-minted, so they carry no information the
/// comparison may rely on.
fn mask_blank_nodes(row: &Value) -> String {
    let obj: serde_json::Map<String, Value> = row
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, val)| {
            let val = normalize(val);
            if bnode_label(&val).is_some() {
                (k, json!({ "type": "bnode", "value": "?" }))
            } else {
                (k, val)
            }
        })
        .collect();
    Value::Object(obj).to_string()
}

/// How many distinct blank nodes a row set uses. Equal counts are necessary
/// for a bijection to exist, so unequal ones name the reason it failed.
fn distinct_blank_nodes(rows: &[Value]) -> usize {
    rows.iter()
        .filter_map(Value::as_object)
        .flat_map(|o| o.values())
        .filter_map(bnode_label)
        .collect::<HashSet<_>>()
        .len()
}

fn bnode_label(v: &Value) -> Option<&str> {
    (v.get("type").and_then(Value::as_str) == Some("bnode"))
        .then(|| v.get("value").and_then(Value::as_str))
        .flatten()
}

/// Is there a bijection between the answer's blank-node labels and the expected
/// rows' that turns one row multiset into the other?
///
/// ponytail: plain backtracking with a step budget, which is ample for W3C
/// fixtures (a handful of rows, one or two blank nodes) and short-circuits
/// entirely when neither side has a blank node. Exhausting the budget grades
/// the case as a failure, never a pass. Upgrade path if some future suite needs
/// it: match on masked-row groups first, or canonicalize the rows the way
/// `oxrdf::Graph::canonicalize` does for graphs.
fn match_blank_nodes(got: &[Value], want: &[Value]) -> bool {
    let has_bnode = |r: &&Value| {
        r.as_object()
            .is_some_and(|o| o.values().any(|v| bnode_label(v).is_some()))
    };
    if !got.iter().chain(want.iter()).any(|r| has_bnode(&r)) {
        return true;
    }
    let mut used = vec![false; want.len()];
    let mut budget = 200_000usize;
    backtrack(
        0,
        got,
        want,
        &mut used,
        &mut HashMap::new(),
        &mut HashMap::new(),
        &mut budget,
    )
}

/// Pair answer row `i` with some still-unused expected row, then recurse.
fn backtrack(
    i: usize,
    got: &[Value],
    want: &[Value],
    used: &mut [bool],
    fwd: &mut HashMap<String, String>,
    rev: &mut HashMap<String, String>,
    budget: &mut usize,
) -> bool {
    if i == got.len() {
        return true;
    }
    for j in 0..want.len() {
        if used[j] {
            continue;
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let mut added = Vec::new();
        if pair_rows(&got[i], &want[j], fwd, rev, &mut added) {
            used[j] = true;
            if backtrack(i + 1, got, want, used, fwd, rev, budget) {
                return true;
            }
            used[j] = false;
        }
        for (a, b) in added {
            fwd.remove(&a);
            rev.remove(&b);
        }
    }
    false
}

/// Can these two rows be the same row under the bijection built so far? Blank
/// node pairs this call introduces are pushed onto `added` so the caller can
/// undo them when the branch fails.
fn pair_rows(
    g: &Value,
    w: &Value,
    fwd: &mut HashMap<String, String>,
    rev: &mut HashMap<String, String>,
    added: &mut Vec<(String, String)>,
) -> bool {
    let (Some(go), Some(wo)) = (g.as_object(), w.as_object()) else {
        return false;
    };
    if go.len() != wo.len() {
        return false;
    }
    for (k, gv) in go {
        let Some(wv) = wo.get(k) else { return false };
        let (gv, wv) = (normalize(gv.clone()), normalize(wv.clone()));
        let pair = (
            bnode_label(&gv).map(str::to_owned),
            bnode_label(&wv).map(str::to_owned),
        );
        match pair {
            (Some(a), Some(b)) => match (fwd.get(&a), rev.get(&b)) {
                (None, None) => {
                    fwd.insert(a.clone(), b.clone());
                    rev.insert(b.clone(), a.clone());
                    added.push((a, b));
                }
                // Already paired: it must be paired with *this* partner.
                (Some(x), Some(y)) if x == &b && y == &a => {}
                _ => return false,
            },
            (None, None) => {
                if gv != wv {
                    return false;
                }
            }
            // A blank node is never equal to a non-blank term.
            _ => return false,
        }
    }
    true
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

    /// Build the SPARQL-JSON an engine answer would serialize to.
    fn answer(vars: &[&str], rows: &[Value]) -> String {
        json!({ "head": { "vars": vars }, "results": { "bindings": rows } }).to_string()
    }

    fn uri(v: &str) -> Value {
        json!({ "type": "uri", "value": v })
    }

    fn bnode(v: &str) -> Value {
        json!({ "type": "bnode", "value": v })
    }

    // ── Blank-node bijection ────────────────────────────────────────────────

    #[test]
    fn blank_nodes_pair_by_bijection_not_by_label() {
        // Same shape, different labels: a pass.
        let got = answer(&["a", "b"], &[json!({ "a": bnode("x"), "b": bnode("x") })]);
        let want = vec![json!({ "a": bnode("b0"), "b": bnode("b0") })];
        assert_eq!(
            compare_solutions(&got, &["a".into(), "b".into()], &want),
            None
        );

        // Different *sharing*: the answer uses two nodes where one is wanted.
        // Labels alone cannot tell these apart — the bijection can.
        let got = answer(&["a", "b"], &[json!({ "a": bnode("x"), "b": bnode("y") })]);
        let why = compare_solutions(&got, &["a".into(), "b".into()], &want)
            .expect("sharing pattern differs");
        assert!(why.contains("no one-to-one renaming"), "{why}");

        // A blank node never matches a non-blank term.
        let got = answer(
            &["a", "b"],
            &[json!({ "a": bnode("x"), "b": uri("http://x") })],
        );
        assert!(compare_solutions(&got, &["a".into(), "b".into()], &want).is_some());
    }

    // ── `.tsv` ──────────────────────────────────────────────────────────────

    /// TSV keeps full term syntax, so it is graded as strictly as `.srx`:
    /// the datatype is part of the answer, not decoration.
    #[test]
    fn tsv_rejects_a_wrong_answer_and_keeps_datatypes() {
        let tsv = "?s\t?o\n<http://ex/s>\t\"4\"^^<http://www.w3.org/2001/XMLSchema#integer>\n";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.tsv");
        std::fs::write(&path, tsv).expect("write");
        let Some(Expected::Solutions { vars, rows }) = read_expected(&path).expect("parse") else {
            panic!("expected a TSV result set");
        };

        let lit = |dt: &str, v: &str| json!({ "type": "literal", "value": v, "datatype": dt });
        let right = answer(
            &["s", "o"],
            &[json!({ "s": uri("http://ex/s"), "o": lit(&format!("{XSD}integer"), "4") })],
        );
        assert_eq!(compare_solutions(&right, &vars, &rows), None);

        // Wrong value.
        let wrong = answer(
            &["s", "o"],
            &[json!({ "s": uri("http://ex/s"), "o": lit(&format!("{XSD}integer"), "5") })],
        );
        assert!(compare_solutions(&wrong, &vars, &rows).is_some());

        // Right lexical form, wrong datatype — CSV would miss this, TSV must not.
        let mistyped = answer(
            &["s", "o"],
            &[json!({ "s": uri("http://ex/s"), "o": lit(&format!("{XSD}decimal"), "4") })],
        );
        assert!(compare_solutions(&mistyped, &vars, &rows).is_some());
    }

    // ── `.csv` ──────────────────────────────────────────────────────────────

    #[test]
    fn csv_reader_handles_quotes_commas_and_crlf() {
        let recs = read_csv_records("a,b\r\n\"x,1\",\"he said \"\"hi\"\"\"\r\n");
        assert_eq!(
            recs,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["x,1".to_string(), "he said \"hi\"".to_string()],
            ]
        );
    }

    /// The guard against a too-lenient CSV grader: it drops datatypes, but it
    /// must still reject a genuinely different value, a missing row, and a
    /// wrong variable.
    #[test]
    fn csv_rejects_a_wrong_answer_even_though_it_ignores_datatypes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.csv");
        std::fs::write(&path, "s,o\r\nhttp://ex/s,4\r\nhttp://ex/t,\r\n").expect("write");
        let Some(Expected::Csv { vars, rows }) = read_expected(&path).expect("parse") else {
            panic!("expected a CSV result set");
        };

        let lit = |dt: &str, v: &str| json!({ "type": "literal", "value": v, "datatype": dt });
        let row = |s: &str, o: Option<Value>| match o {
            Some(o) => json!({ "s": uri(s), "o": o }),
            None => json!({ "s": uri(s) }),
        };

        // Right values: a pass.
        let right = answer(
            &["s", "o"],
            &[
                row("http://ex/s", Some(lit(&format!("{XSD}integer"), "4"))),
                row("http://ex/t", None),
            ],
        );
        assert_eq!(compare_csv(&right, &vars, &rows), None);

        // Wrong value: still caught, though the datatype is not compared.
        let wrong = answer(
            &["s", "o"],
            &[
                row("http://ex/s", Some(lit(&format!("{XSD}integer"), "5"))),
                row("http://ex/t", None),
            ],
        );
        assert!(compare_csv(&wrong, &vars, &rows).is_some());

        // Missing row, and a wrong variable name.
        let short = answer(
            &["s", "o"],
            &[row("http://ex/s", Some(lit(&format!("{XSD}integer"), "4")))],
        );
        assert!(compare_csv(&short, &vars, &rows).is_some());
        assert!(compare_csv(&right, &["s".into(), "p".into()], &rows).is_some());
    }

    /// The documented blind spot, asserted so it cannot drift silently: CSV
    /// carries no types, so a differently-typed term with the same text is
    /// accepted. `.srx`/`.srj`/`.tsv` are what catch these.
    #[test]
    fn csv_cannot_see_datatype_language_or_iri_vs_literal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.csv");
        std::fs::write(&path, "o\r\n4\r\n").expect("write");
        let Some(Expected::Csv { vars, rows }) = read_expected(&path).expect("parse") else {
            panic!("expected a CSV result set");
        };
        for term in [
            json!({ "type": "literal", "value": "4", "datatype": format!("{XSD}integer") }),
            json!({ "type": "literal", "value": "4", "datatype": format!("{XSD}decimal") }),
            json!({ "type": "literal", "value": "4", "xml:lang": "en" }),
            json!({ "type": "uri", "value": "4" }),
        ] {
            let got = answer(&["o"], &[json!({ "o": term })]);
            assert_eq!(compare_csv(&got, &vars, &rows), None);
        }
    }

    // ── `.ttl` ──────────────────────────────────────────────────────────────

    #[test]
    fn construct_graphs_compare_up_to_blank_node_renaming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r.ttl");
        std::fs::write(
            &path,
            "<http://ex/s> <http://ex/p> _:x . _:x <http://ex/q> \"v\" .",
        )
        .expect("write");
        let Some(Expected::Graph(want)) = read_expected(&path).expect("parse") else {
            panic!("expected a graph result");
        };
        let t = |s: &str, p: &str, o: &str| (s.to_string(), p.to_string(), o.to_string());

        // Same graph, a different blank-node label: a pass.
        let right = [
            t("http://ex/s", "http://ex/p", "_:b7"),
            t("_:b7", "http://ex/q", "\"v\""),
        ];
        assert_eq!(compare_graph(&right, &want, &path), None);

        // Two blank nodes where one is shared: not isomorphic.
        let split = [
            t("http://ex/s", "http://ex/p", "_:b7"),
            t("_:b8", "http://ex/q", "\"v\""),
        ];
        assert!(compare_graph(&split, &want, &path).is_some());

        // A wrong object value, and a missing triple.
        let wrong = [
            t("http://ex/s", "http://ex/p", "_:b7"),
            t("_:b7", "http://ex/q", "\"other\""),
        ];
        assert!(compare_graph(&wrong, &want, &path).is_some());
        assert!(compare_graph(&right[..1], &want, &path).is_some());
    }
}
