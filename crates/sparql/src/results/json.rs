//! SPARQL 1.1 Query Results JSON Format.
//! <https://www.w3.org/TR/sparql11-results-json/>

use crate::algebra::Term;
use crate::exec::runtime::literal_parts;
use crate::exec::Bindings;
use serde_json::{json, Map, Value};

pub fn write_select_json(vars: &[String], rows: &[Bindings]) -> String {
    let bindings: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut obj = Map::new();
            for v in vars {
                if let Some(t) = row.get(v) {
                    obj.insert(v.clone(), term_to_json(t));
                }
            }
            Value::Object(obj)
        })
        .collect();

    json!({
        "head": { "vars": vars },
        "results": { "bindings": bindings }
    })
    .to_string()
}

pub fn write_ask_json(answer: bool) -> String {
    json!({ "head": {}, "boolean": answer }).to_string()
}

fn term_to_json(t: &Term) -> Value {
    match t {
        Term::Iri(s) => json!({ "type": "uri", "value": s }),
        Term::BlankNode(s) => json!({ "type": "bnode", "value": s.trim_start_matches("_:") }),
        Term::Literal(raw) => parse_literal_to_json(raw),
        Term::Var(_) => json!({ "type": "literal", "value": "<unbound>" }),
        // RDF 1.2 ground triple terms are emitted by the SPARQL 1.2 JSON
        // results format as `{ "type": "triple", "value": { … } }`. The
        // Stage-1 results path holds terms as opaque strings and has no
        // pattern carrier here; emit the SPARQL 1.2 "unbound triple-term"
        // shape until SPEC-07 RDF 1.2 follow-up wires real serialisation.
        Term::Triple(_) => {
            json!({ "type": "literal", "value": "<rdf-12-triple-term unsupported>" })
        }
    }
}

/// Parse an N-Triples-form literal (`"foo"`, `"foo"@lang`,
/// `"foo"^^<datatype>`) into a SPARQL-JSON binding, reusing the crate's
/// shared lexical splitter so the JSON/XML/runtime paths stay in lockstep.
fn parse_literal_to_json(raw: &str) -> Value {
    let (value, lang, datatype) = literal_parts(raw);
    match (lang, datatype) {
        (Some(lang), _) => json!({ "type": "literal", "value": value, "xml:lang": lang }),
        (_, Some(dt)) => json!({ "type": "literal", "value": value, "datatype": dt }),
        _ => json!({ "type": "literal", "value": value }),
    }
}
