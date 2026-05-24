//! SPARQL CSV results. https://www.w3.org/TR/sparql11-results-csv-tsv/

use crate::algebra::Term;
use crate::exec::Bindings;

pub fn write_select_csv(vars: &[String], rows: &[Bindings]) -> String {
    let mut out = String::new();
    out.push_str(&vars.join(","));
    out.push_str("\r\n");
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

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
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
