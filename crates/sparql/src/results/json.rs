//! SPARQL 1.1 Query Results JSON Format.
//! <https://www.w3.org/TR/sparql11-results-json/>

use crate::algebra::Term;
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
    }
}

/// Parse an N-Triples-form literal into a SPARQL-JSON binding.
/// Recognises `"foo"`, `"foo"@lang`, `"foo"^^<datatype>`.
fn parse_literal_to_json(raw: &str) -> Value {
    // Best-effort lexical parsing; sufficient for the W3C subset.
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return json!({ "type": "literal", "value": raw });
    }
    let mut end_quote = None;
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            end_quote = Some(i);
            break;
        }
        i += 1;
    }
    let Some(eq) = end_quote else {
        return json!({ "type": "literal", "value": raw });
    };
    let value = &raw[1..eq];
    let tail = &raw[eq + 1..];

    if let Some(rest) = tail.strip_prefix("@") {
        return json!({ "type": "literal", "value": value, "xml:lang": rest });
    }
    if let Some(rest) = tail.strip_prefix("^^") {
        let dt = rest.trim_start_matches('<').trim_end_matches('>');
        return json!({
            "type": "literal",
            "value": value,
            "datatype": dt
        });
    }
    json!({ "type": "literal", "value": value })
}
