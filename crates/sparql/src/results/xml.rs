//! SPARQL 1.1 Query Results XML Format.
//! <https://www.w3.org/TR/rdf-sparql-XMLres/>
//!
//! Hand-rolled rather than delegated to `sparesults` because the
//! Stage-1 `MemStore` erases term kinds on scan (every bound value
//! arrives as `Term::Iri(lexical)`), so we cannot hand `sparesults`
//! a faithful `oxrdf::Term`. We emit best-effort `<uri>` / `<literal>`
//! / `<bnode>` element types from the lexical form, reusing the same
//! literal-parsing logic the JSON writer uses.

use crate::algebra::Term;
use crate::exec::runtime::literal_parts;
use crate::exec::Bindings;
use crate::results::SelectSerializer;

/// Incremental SPARQL-XML SELECT serializer. Stateless: `<result>` blocks
/// are self-contained, so chunks need no cross-chunk bookkeeping.
pub struct XmlSelectSerializer;

impl SelectSerializer for XmlSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        out.push_str(r#"<?xml version="1.0"?>"#);
        out.push_str("\n<sparql xmlns=\"http://www.w3.org/2005/sparql-results#\">\n");
        out.push_str("  <head>\n");
        for v in vars {
            out.push_str(&format!(
                "    <variable name=\"{}\"/>\n",
                xml_attr_escape(v)
            ));
        }
        out.push_str("  </head>\n");
        out.push_str("  <results>\n");
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            out.push_str("    <result>\n");
            for v in vars {
                if let Some(t) = row.get(v) {
                    out.push_str(&format!("      <binding name=\"{}\">", xml_attr_escape(v)));
                    out.push_str(&term_to_xml(t));
                    out.push_str("</binding>\n");
                }
            }
            out.push_str("    </result>\n");
        }
        out
    }

    fn footer(&mut self) -> String {
        "  </results>\n</sparql>\n".to_string()
    }
}

/// Serialise a SELECT result set as SPARQL Results XML.
pub fn write_select_xml(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = XmlSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}

/// Serialise an ASK result as SPARQL Results XML.
pub fn write_ask_xml(answer: bool) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0"?>"#);
    out.push_str("\n<sparql xmlns=\"http://www.w3.org/2005/sparql-results#\">\n");
    out.push_str("  <head/>\n");
    out.push_str(&format!("  <boolean>{}</boolean>\n", answer));
    out.push_str("</sparql>\n");
    out
}

fn term_to_xml(t: &Term) -> String {
    match t {
        Term::Iri(s) => format!("<uri>{}</uri>", xml_text_escape(s)),
        Term::BlankNode(s) => {
            format!(
                "<bnode>{}</bnode>",
                xml_text_escape(s.trim_start_matches("_:"))
            )
        }
        Term::Literal(raw) => parse_literal_to_xml(raw),
        // An unbound var should not appear here (the caller only emits
        // bound bindings), but be defensive.
        Term::Var(_) => "<literal></literal>".to_string(),
        // RDF 1.2 ground triple terms have no Stage-1 lexical carrier.
        Term::Triple(_) => "<literal></literal>".to_string(),
    }
}

/// Parse an N-Triples-form literal (`"foo"`, `"foo"@lang`,
/// `"foo"^^<datatype>`) into a SPARQL-XML `<literal>` element, reusing the
/// crate's shared lexical splitter (the JSON writer uses the same one).
fn parse_literal_to_xml(raw: &str) -> String {
    let (value, lang, datatype) = literal_parts(raw);
    if let Some(lang) = lang {
        format!(
            "<literal xml:lang=\"{}\">{}</literal>",
            xml_attr_escape(&lang),
            xml_text_escape(&value)
        )
    } else if let Some(dt) = datatype {
        format!(
            "<literal datatype=\"{}\">{}</literal>",
            xml_attr_escape(&dt),
            xml_text_escape(&value)
        )
    } else {
        format!("<literal>{}</literal>", xml_text_escape(&value))
    }
}

fn xml_text_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_attr_escape(s: &str) -> String {
    xml_text_escape(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Term)]) -> Bindings {
        let mut b = Bindings::new();
        for (k, t) in pairs {
            b.set((*k).to_owned(), t.clone());
        }
        b
    }

    #[test]
    fn select_xml_is_well_formed_and_roundtrips() {
        let vars = vec!["s".to_string(), "o".to_string()];
        let rows = vec![
            row(&[
                ("s", Term::Iri("http://example/a".into())),
                ("o", Term::Literal("\"hello\"".into())),
            ]),
            row(&[("s", Term::Iri("http://example/b".into()))]),
        ];
        let xml = write_select_xml(&vars, &rows);

        // Parse it back with oxrdf's sparesults to prove well-formedness
        // and binding round-trip.
        use sparesults::{QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput};
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Xml);
        let out = parser
            .for_reader(xml.as_bytes())
            .expect("parse XML results");
        match out {
            ReaderQueryResultsParserOutput::Solutions(solutions) => {
                let collected: Vec<_> = solutions.map(|s| s.expect("solution")).collect();
                assert_eq!(collected.len(), 2);
                // first row binds both s and o
                assert!(collected[0].get("s").is_some());
                assert!(collected[0].get("o").is_some());
                // second row binds only s
                assert!(collected[1].get("s").is_some());
                assert!(collected[1].get("o").is_none());
            }
            ReaderQueryResultsParserOutput::Boolean(_) => panic!("expected solutions"),
        }
    }

    #[test]
    fn ask_xml_roundtrips() {
        let xml = write_ask_xml(true);
        use sparesults::{QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput};
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Xml);
        let out = parser.for_reader(xml.as_bytes()).expect("parse XML");
        match out {
            ReaderQueryResultsParserOutput::Boolean(b) => assert!(b),
            ReaderQueryResultsParserOutput::Solutions(_) => panic!("expected boolean"),
        }
    }

    #[test]
    fn literal_with_datatype_and_lang() {
        let vars = vec!["v".to_string()];
        let rows = vec![
            row(&[(
                "v",
                Term::Literal("\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>".into()),
            )]),
            row(&[("v", Term::Literal("\"bonjour\"@fr".into()))]),
        ];
        let xml = write_select_xml(&vars, &rows);
        assert!(xml.contains("datatype=\"http://www.w3.org/2001/XMLSchema#integer\""));
        assert!(xml.contains("xml:lang=\"fr\""));
    }
}
