//! SPARQL 1.1 Query Results JSON Format.
//! <https://www.w3.org/TR/sparql11-results-json/>

use crate::algebra::Term;
use crate::exec::runtime::literal_parts;
use crate::exec::Bindings;
use crate::results::SelectSerializer;
use serde_json::{json, Map, Value};

/// Incremental SPARQL-JSON SELECT serializer. The only cross-chunk state is
/// the comma placement between binding objects.
#[derive(Default)]
pub struct JsonSelectSerializer {
    any_rows: bool,
}

impl SelectSerializer for JsonSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        format!(
            "{{\"head\":{{\"vars\":{}}},\"results\":{{\"bindings\":[",
            serde_json::to_string(vars).expect("a Vec<String> always serializes")
        )
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            if self.any_rows {
                out.push(',');
            }
            self.any_rows = true;
            let mut obj = Map::new();
            for v in vars {
                if let Some(t) = row.get(v) {
                    obj.insert(v.clone(), term_to_json(t));
                }
            }
            out.push_str(&Value::Object(obj).to_string());
        }
        out
    }

    fn footer(&mut self) -> String {
        "]}}".to_string()
    }
}

pub fn write_select_json(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = JsonSelectSerializer::default();
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
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
