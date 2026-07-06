//! SPARQL CSV results. <https://www.w3.org/TR/sparql11-results-csv-tsv/>

use crate::algebra::Term;
use crate::exec::Bindings;
use crate::results::SelectSerializer;

/// Incremental SPARQL-CSV SELECT serializer. Stateless: lines are
/// self-contained.
pub struct CsvSelectSerializer;

impl SelectSerializer for CsvSelectSerializer {
    fn header(&mut self, vars: &[String]) -> String {
        let mut out = String::new();
        out.push_str(&vars.join(","));
        out.push_str("\r\n");
        out
    }

    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String {
        let mut out = String::new();
        for row in rows {
            let cells: Vec<String> = vars
                .iter()
                .map(|v| match row.get(v) {
                    None => String::new(),
                    Some(t) => csv_escape(&term_to_lex(t)),
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push_str("\r\n");
        }
        out
    }

    fn footer(&mut self) -> String {
        String::new()
    }
}

pub fn write_select_csv(vars: &[String], rows: &[Bindings]) -> String {
    let mut ser = CsvSelectSerializer;
    let mut out = ser.header(vars);
    out.push_str(&ser.chunk(vars, rows));
    out.push_str(&ser.footer());
    out
}

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
        // RDF 1.2 triple terms in solution mappings: SPARQL 1.1 CSV has
        // no syntax for them. Emit empty per the W3C "unbound" rule;
        // SPEC-07 RDF 1.2 follow-up will route this through a real
        // triple-term encoder.
        Term::Triple(_) => String::new(),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_owned()
    }
}
