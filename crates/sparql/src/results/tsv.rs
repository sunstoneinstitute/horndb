//! SPARQL TSV results. <https://www.w3.org/TR/sparql11-results-csv-tsv/>

use crate::algebra::Term;
use crate::exec::Bindings;
use crate::results::SelectSerializer;

/// Incremental SPARQL-TSV SELECT serializer. Stateless: lines are
/// self-contained.
pub struct TsvSelectSerializer;

impl SelectSerializer for TsvSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        let header: Vec<String> = vars.iter().map(|v| format!("?{v}")).collect();
        out.push_str(&header.join("\t"));
        out.push('\n');
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            let cells: Vec<String> = vars
                .iter()
                .map(|v| match row.get(v) {
                    None => String::new(),
                    Some(Term::Iri(s)) => format!("<{s}>"),
                    Some(Term::BlankNode(s)) => {
                        if s.starts_with("_:") {
                            s.clone()
                        } else {
                            format!("_:{s}")
                        }
                    }
                    Some(Term::Literal(s)) => s.clone(),
                    Some(Term::Var(v)) => format!("?{}", v.name()),
                    // RDF 1.2 triple-term solution-mapping values: TSV has no
                    // canonical encoding; emit empty (the SPARQL 1.1 "unbound"
                    // shape) until SPEC-07 RDF 1.2 follow-up.
                    Some(Term::Triple(_)) => String::new(),
                })
                .collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
        out
    }

    fn footer(&mut self) -> String {
        String::new()
    }
}

pub fn write_select_tsv(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = TsvSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}
