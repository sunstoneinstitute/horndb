//! Chunking invariance for the incremental SELECT serializers (#128 HTTP
//! streaming): header + chunks + footer must byte-equal the one-shot
//! writers for every chunk split, in all four formats.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::Bindings;
use horndb_sparql::results::csv::write_select_csv;
use horndb_sparql::results::json::write_select_json;
use horndb_sparql::results::tsv::write_select_tsv;
use horndb_sparql::results::xml::write_select_xml;
use horndb_sparql::results::{select_serializer, ResultFormat};

/// 5 rows over (?s, ?o); odd rows leave ?o unbound; values exercise CSV
/// quoting (comma), XML escaping (`<`), and language-tagged literals.
fn fixture() -> (Vec<String>, Vec<Bindings>) {
    let vars = vec!["s".to_string(), "o".to_string()];
    let rows = (0..5)
        .map(|i| {
            let mut b = Bindings::new();
            b.set("s", Term::Iri(format!("http://ex/s{i}")));
            if i % 2 == 0 {
                b.set("o", Term::Literal(format!("\"v{i},<x>\"@en")));
            }
            b
        })
        .collect();
    (vars, rows)
}

/// One-shot writer signature shared by all four formats; named to keep the
/// `cases` arrays below under clippy's `type_complexity` threshold.
type OneShotWriter = fn(&[String], &[Bindings]) -> String;

fn incremental(fmt: ResultFormat, vars: &[String], rows: &[Bindings], chunk: usize) -> String {
    let mut ser = select_serializer(fmt);
    let mut out = ser.header(vars);
    for c in rows.chunks(chunk) {
        out.push_str(&ser.chunk(vars, c));
    }
    out.push_str(&ser.footer());
    out
}

#[test]
fn incremental_equals_one_shot_for_every_format_and_chunking() {
    let (vars, rows) = fixture();
    let cases: [(ResultFormat, OneShotWriter); 4] = [
        (ResultFormat::Json, write_select_json),
        (ResultFormat::Xml, write_select_xml),
        (ResultFormat::Csv, write_select_csv),
        (ResultFormat::Tsv, write_select_tsv),
    ];
    for (fmt, one_shot) in cases {
        let expected = one_shot(&vars, &rows);
        for chunk in [1, 2, 3, 5] {
            assert_eq!(
                incremental(fmt, &vars, &rows, chunk),
                expected,
                "{fmt:?} diverges at chunk size {chunk}"
            );
        }
    }
}

#[test]
fn zero_row_stream_is_well_formed() {
    let (vars, _) = fixture();
    let cases: [(ResultFormat, OneShotWriter); 4] = [
        (ResultFormat::Json, write_select_json),
        (ResultFormat::Xml, write_select_xml),
        (ResultFormat::Csv, write_select_csv),
        (ResultFormat::Tsv, write_select_tsv),
    ];
    for (fmt, one_shot) in cases {
        let mut ser = select_serializer(fmt);
        let mut out = ser.header(&vars);
        out.push_str(&ser.footer());
        assert_eq!(out, one_shot(&vars, &[]), "{fmt:?} empty result diverges");
    }
}
