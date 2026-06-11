//! End-to-end tests for the expanded expression surface (#66):
//! arithmetic, IF, COALESCE, builtin functions.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn store_with_prices() -> MemStore {
    let mut s = MemStore::default();
    for (subj, price) in [("a", 4), ("b", 11)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{subj}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    s
}

fn rows(q: &str, s: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, s).expect("query should run") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Lexical value of a binding, ignoring term kind and literal decoration.
/// Note: does not handle escaped quotes inside lexical values (fine for these fixtures).
fn lexical(b: &horndb_sparql::exec::Bindings, var: &str) -> String {
    let t = b.get(var).unwrap_or_else(|| panic!("unbound ?{var}"));
    let raw = match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        other => panic!("unexpected term {other:?}"),
    };
    if let Some(stripped) = raw.strip_prefix('"') {
        stripped.split('"').next().unwrap().to_owned()
    } else {
        raw
    }
}

#[test]
fn bind_arithmetic_add() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?y WHERE { ?s <http://example.org/price> ?p . BIND(?p + 1 AS ?y) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "y")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "5".into()),
            ("http://example.org/b".into(), "12".into()),
        ]
    );
}

#[test]
fn filter_arithmetic_comparison() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s WHERE { ?s <http://example.org/price> ?p . FILTER(?p * 2 > 10) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/b");
}

#[test]
fn division_yields_decimal_and_div_by_zero_drops_row() {
    let s = store_with_prices();
    // 4 / 2 = 2 ; 11 / 2 = 5.5 — both rows keep a bound ?h.
    let got = rows(
        "SELECT ?h WHERE { ?s <http://example.org/price> ?p . BIND(?p / 2 AS ?h) }",
        &s,
    );
    let mut vals: Vec<String> = got.iter().map(|b| lexical(b, "h")).collect();
    vals.sort();
    assert_eq!(vals, vec!["2".to_string(), "5.5".to_string()]);
    // Division by zero is an expression error: BIND leaves ?z unbound.
    let got = rows(
        "SELECT ?s ?z WHERE { ?s <http://example.org/price> ?p . BIND(?p / 0 AS ?z) }",
        &s,
    );
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|b| b.get("z").is_none()));
}

#[test]
fn unary_minus() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?n WHERE { ?s <http://example.org/price> ?p . BIND(-?p AS ?n) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "n"), "-4");
}

#[test]
fn if_in_bind() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s ?label WHERE { ?s <http://example.org/price> ?p . \
         BIND(IF(?p > 10, \"expensive\", \"cheap\") AS ?label) }",
        &s,
    );
    let mut pairs: Vec<(String, String)> = got
        .iter()
        .map(|b| (lexical(b, "s"), lexical(b, "label")))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("http://example.org/a".into(), "cheap".into()),
            ("http://example.org/b".into(), "expensive".into()),
        ]
    );
}

#[test]
fn coalesce_picks_first_bound() {
    let s = store_with_prices();
    // ?unbound never binds; COALESCE falls through to ?p.
    let got = rows(
        "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
         OPTIONAL { ?s <http://example.org/missing> ?unbound } \
         BIND(COALESCE(?unbound, ?p) AS ?v) FILTER(?s = <http://example.org/a>) }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "v"), "4");
}

#[test]
fn sum_of_products_aggregate() {
    let mut s = MemStore::default();
    for (o, qty, price) in [("o1", 2, 3), ("o2", 5, 4)] {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/qty".into()),
            Term::Literal(format!("\"{qty}\"^^<{XSD_INT}>")),
        );
        s.insert_triple(
            Term::Iri(format!("http://example.org/{o}")),
            Term::Iri("http://example.org/price".into()),
            Term::Literal(format!("\"{price}\"^^<{XSD_INT}>")),
        );
    }
    let got = rows(
        "SELECT (SUM(?q * ?p) AS ?total) WHERE { \
         ?o <http://example.org/qty> ?q . ?o <http://example.org/price> ?p }",
        &s,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "total"), "26");
}

fn store_with_names() -> MemStore {
    let mut s = MemStore::default();
    let data = [
        ("a", "\"Alice\"@en"),
        ("b", "\"bob\""),
        ("c", "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
    ];
    for (subj, lit) in data {
        s.insert_triple(
            Term::Iri(format!("http://example.org/{subj}")),
            Term::Iri("http://example.org/name".into()),
            Term::Literal(lit.to_owned()),
        );
    }
    s
}

#[test]
fn string_functions() {
    let s = store_with_names();
    let q = "SELECT ?s ?len ?up WHERE { ?s <http://example.org/name> ?n . \
             BIND(STRLEN(?n) AS ?len) BIND(UCASE(?n) AS ?up) \
             FILTER(?s = <http://example.org/b>) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "len"), "3");
    assert_eq!(lexical(&got[0], "up"), "BOB");
}

#[test]
fn substr_and_concat() {
    let s = store_with_names();
    let q = "SELECT ?x WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(CONCAT(SUBSTR(?n, 1, 2), \"!\") AS ?x) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "x"), "Al!");
}

#[test]
fn str_starts_ends_contains_before_after() {
    let s = store_with_names();
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(STRSTARTS(?n, \"Al\") && STRENDS(?n, \"ce\") && CONTAINS(?n, \"lic\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");

    let q = "SELECT ?b ?a WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(STRBEFORE(?n, \"i\") AS ?b) BIND(STRAFTER(?n, \"i\") AS ?a) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "b"), "Al");
    assert_eq!(lexical(&got[0], "a"), "ce");
}

#[test]
fn regex_and_replace() {
    let s = store_with_names();
    // Case-insensitive match.
    let q =
        "SELECT ?s WHERE { ?s <http://example.org/name> ?n . FILTER(REGEX(?n, \"^ali\", \"i\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");
    // Invalid pattern is an expression error: filter drops all rows.
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . FILTER(REGEX(?n, \"(\")) }";
    assert_eq!(rows(q, &s).len(), 0);
    // REPLACE with a capture group.
    let q = "SELECT ?x WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/b>) \
             BIND(REPLACE(?n, \"b(o)\", \"B$1\") AS ?x) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "x"), "Bob");
}

#[test]
fn str_lang_datatype() {
    let s = store_with_names();
    let q = "SELECT ?lang ?dt WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/a>) \
             BIND(LANG(?n) AS ?lang) BIND(DATATYPE(?n) AS ?dt) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "lang"), "en");
    assert_eq!(
        lexical(&got[0], "dt"),
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
    );
    let q = "SELECT ?dt WHERE { ?s <http://example.org/name> ?n . \
             FILTER(?s = <http://example.org/c>) BIND(DATATYPE(?n) AS ?dt) }";
    let got = rows(q, &s);
    assert_eq!(
        lexical(&got[0], "dt"),
        "http://www.w3.org/2001/XMLSchema#integer"
    );
    // LANGMATCHES
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(LANGMATCHES(LANG(?n), \"en\")) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/a");
}

#[test]
fn numeric_functions() {
    let s = store_with_prices();
    let q = "SELECT ?abs ?ceil ?floor ?round WHERE { \
             <http://example.org/a> <http://example.org/price> ?p . \
             BIND(ABS(0 - ?p) AS ?abs) BIND(CEIL(?p / 2) AS ?ceil) \
             BIND(FLOOR(?p / 2) AS ?floor) BIND(ROUND(?p / 2) AS ?round) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "abs"), "4"); // |0-4| = 4
    assert_eq!(lexical(&got[0], "ceil"), "2"); // ceil(2.0)
    assert_eq!(lexical(&got[0], "floor"), "2"); // floor(2.0)
    assert_eq!(lexical(&got[0], "round"), "2"); // round(2.0)
}

#[test]
fn type_check_functions() {
    let s = store_with_names();
    // isLITERAL on a literal-valued object; isIRI on the subject.
    let q = "SELECT ?s WHERE { ?s <http://example.org/name> ?n . \
             FILTER(ISLITERAL(?n) && ISIRI(?s) && !ISBLANK(?s)) \
             FILTER(?s = <http://example.org/c>) FILTER(ISNUMERIC(?n)) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "s"), "http://example.org/c");
}

#[test]
fn datetime_accessors() {
    let mut s = MemStore::default();
    s.insert_triple(
        Term::Iri("http://example.org/e".into()),
        Term::Iri("http://example.org/at".into()),
        Term::Literal(
            "\"2026-06-10T12:34:56\"^^<http://www.w3.org/2001/XMLSchema#dateTime>".into(),
        ),
    );
    let q = "SELECT ?y ?mo ?d ?h ?mi ?sec WHERE { ?e <http://example.org/at> ?t . \
             BIND(YEAR(?t) AS ?y) BIND(MONTH(?t) AS ?mo) BIND(DAY(?t) AS ?d) \
             BIND(HOURS(?t) AS ?h) BIND(MINUTES(?t) AS ?mi) BIND(SECONDS(?t) AS ?sec) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert_eq!(lexical(&got[0], "y"), "2026");
    assert_eq!(lexical(&got[0], "mo"), "6");
    assert_eq!(lexical(&got[0], "d"), "10");
    assert_eq!(lexical(&got[0], "h"), "12");
    assert_eq!(lexical(&got[0], "mi"), "34");
    assert_eq!(lexical(&got[0], "sec"), "56");
}

#[test]
fn round_half_rounds_toward_positive_infinity() {
    let mut s = MemStore::default();
    s.insert_triple(
        Term::Iri("http://example.org/x".into()),
        Term::Iri("http://example.org/v".into()),
        Term::Literal("\"2.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>".into()),
    );
    let q = "SELECT ?r ?nr WHERE { ?x <http://example.org/v> ?v . \
             BIND(ROUND(?v) AS ?r) BIND(ROUND(-?v) AS ?nr) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "r"), "3");
    assert_eq!(lexical(&got[0], "nr"), "-2");
}

#[test]
fn string_builtins_see_unescaped_lexical_values() {
    let mut s = MemStore::default();
    // Stored N-Triples form "a\nb" — lexical value is a, newline, b.
    s.insert_triple(
        Term::Iri("http://example.org/x".into()),
        Term::Iri("http://example.org/v".into()),
        Term::Literal("\"a\\nb\"".into()),
    );
    let q = "SELECT ?l ?up WHERE { ?x <http://example.org/v> ?v . \
             BIND(STRLEN(?v) AS ?l) BIND(UCASE(?v) AS ?up) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "l"), "3"); // not 4: \n is one char
                                            // UCASE round-trips the escape: stored form is "A\nB".
    assert_eq!(got[0].get("up"), Some(&Term::Literal("\"A\\nB\"".into())));
}

#[test]
fn if_condition_uses_effective_boolean_value() {
    let s = store_with_prices();
    // A boolean false literal as the condition takes the else branch.
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(IF(\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>, \"t\", \"f\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "f");
    // Numeric zero is false; non-zero is true.
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(IF(0, \"t\", \"f\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "f");
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(IF(?p, \"t\", \"f\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "t"); // ?p = 4 → truthy
}

#[test]
fn graph_iri_lowers_to_inner_pattern() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?s WHERE { GRAPH <http://example.org/g> { ?s <http://example.org/price> ?p } }",
        &s,
    );
    // Stage-1 merged-graph semantics: GRAPH is transparent.
    assert_eq!(got.len(), 2);
}

#[test]
fn graph_var_lowers_with_unbound_graph_var() {
    let s = store_with_prices();
    let got = rows(
        "SELECT ?g ?s WHERE { GRAPH ?g { ?s <http://example.org/price> ?p } }",
        &s,
    );
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|b| b.get("g").is_none()));
}

#[test]
fn ebv_is_datatype_aware() {
    let s = store_with_prices();
    // A plain string "0" is a non-empty string: EBV true (§17.2.2) —
    // boolean/numeric rules apply only to boolean/numeric datatypes.
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(IF(\"0\", \"t\", \"f\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "t");
    // The empty string is false.
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(IF(\"\", \"t\", \"f\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "f");
}

#[test]
fn isnumeric_requires_numeric_datatype() {
    let s = store_with_prices();
    // A plain string that merely looks numeric is NOT numeric (§17.4.2.4).
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(ISNUMERIC(\"42\") AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "false");
    // A typed numeric literal is.
    let q = "SELECT ?v WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(ISNUMERIC(?p) AS ?v) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "v"), "true");
}

#[test]
fn select_star_keeps_graph_var_visible() {
    let s = store_with_prices();
    let got = execute_query(
        "SELECT * WHERE { GRAPH ?g { ?s <http://example.org/price> ?p } }",
        &s,
    )
    .expect("query should run");
    match got {
        QueryAnswer::Solutions { vars, rows } => {
            assert!(
                vars.iter().any(|v| v == "g"),
                "?g missing from head.vars: {vars:?}"
            );
            assert!(vars.iter().any(|v| v == "s"));
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|b| b.get("g").is_none()));
        }
        other => panic!("expected solutions, got {other:?}"),
    }
}

#[test]
fn unescape_covers_all_ntriples_echars() {
    let mut s = MemStore::default();
    // Stored escaped form covers \b and \f ECHARs: lexical value is
    // backspace + form feed (2 chars).
    s.insert_triple(
        Term::Iri("http://example.org/x".into()),
        Term::Iri("http://example.org/v".into()),
        Term::Literal("\"\\b\\f\"".into()),
    );
    let q = "SELECT ?l WHERE { ?x <http://example.org/v> ?v . BIND(STRLEN(?v) AS ?l) }";
    let got = rows(q, &s);
    assert_eq!(lexical(&got[0], "l"), "2");
}

#[test]
fn datetime_accessors_require_datetime_datatype() {
    let s = store_with_prices();
    // A plain string that looks like a timestamp is a type error:
    // the BIND leaves ?y unbound.
    let q = "SELECT ?s ?y WHERE { ?s <http://example.org/price> ?p . \
             FILTER(?s = <http://example.org/a>) \
             BIND(YEAR(\"2026-06-10T12:34:56\") AS ?y) }";
    let got = rows(q, &s);
    assert_eq!(got.len(), 1);
    assert!(got[0].get("y").is_none());
}
