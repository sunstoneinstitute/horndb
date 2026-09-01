//! HDB-100: `eval_group_native`'s column-bound fast paths (COUNT /
//! COUNT(DISTINCT) / SUM / AVG / MIN / MAX over a bare scan-column variable)
//! against a real `HornBackend` — i.e. `Slot::Id` member columns, so these
//! actually exercise `Executor::decode_numeric`/`decode_terms`, not just the
//! `Slot::Term` path `exec_aggregate.rs`'s `MemStore` tests already cover.
//!
//! Every query here mixes a `COUNT`/`COUNT DISTINCT` with a `SUM`/`MIN`/`MAX`
//! (or otherwise fails `plan::pushdown::lower_count_group`'s plain-count-only
//! test) so it is guaranteed to run through `eval_group_native`, not the
//! `GroupCountScan` pushdown — the same shape as trainmarks q2/q4.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{Bindings, Store};

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}
fn dbl(v: &str) -> Term {
    Term::Literal(format!(
        "\"{v}\"^^<http://www.w3.org/2001/XMLSchema#double>"
    ))
}
fn plain(s: &str) -> Term {
    Term::Literal(format!("\"{s}\""))
}

fn solutions(q: &str, store: &HornBackend) -> Vec<Bindings> {
    match execute_query(q, store).expect("query") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Strip a literal down to its bare value (no quotes/datatype) — for
/// integer/double results whose exact datatype isn't under test.
fn val(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::BlankNode(s) => s.clone(),
        Term::Literal(raw) => {
            let raw = raw.trim();
            if let Some(rest) = raw.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    return rest[..end].to_owned();
                }
            }
            raw.to_owned()
        }
        other => panic!("unexpected term {other:?}"),
    }
}

/// The full N-Triples-ish lexical form, quotes/datatype/lang included — for
/// pinning that MIN/MAX return the *original* term, not a recomputed one.
fn raw(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        other => panic!("unexpected term {other:?}"),
    }
}

#[test]
fn fast_count_sum_avg_over_scan_columns() {
    // Single-key GROUP BY over a scanned (Slot::Id) column — exercises the
    // Do-3 scalar-key grouping path together with the Do-2 COUNT/SUM/AVG
    // fast paths. One row's ?v is unbound (no <amount> triple) so COUNT must
    // skip it while SUM/AVG must still fold correctly over the bound rest.
    let mut st = HornBackend::new();
    for (e, cat) in [("e1", "a"), ("e2", "a"), ("e3", "a"), ("e4", "b")] {
        st.insert_triple(iri(e), iri("cat"), iri(cat));
    }
    st.insert_triple(iri("e1"), iri("amount"), dbl("10.0"));
    st.insert_triple(iri("e2"), iri("amount"), dbl("20.0"));
    // e3 has no <amount> triple: OPTIONAL leaves ?v Unbound for it.
    st.insert_triple(iri("e4"), iri("amount"), dbl("5.0"));

    let rows = solutions(
        "SELECT ?cat (COUNT(?v) AS ?c) (SUM(?v) AS ?s) (AVG(?v) AS ?avg) WHERE { \
         ?e <http://ex/cat> ?cat . OPTIONAL { ?e <http://ex/amount> ?v } } \
         GROUP BY ?cat ORDER BY ?cat",
        &st,
    );
    assert_eq!(rows.len(), 2, "two categories");

    // cat a: bound amounts {10, 20}, one unbound (e3) — COUNT=2, SUM=30, AVG=15.
    assert_eq!(val(rows[0].get("cat").unwrap()), "http://ex/a");
    assert_eq!(
        val(rows[0].get("c").unwrap()),
        "2",
        "COUNT skips the unbound member"
    );
    assert_eq!(val(rows[0].get("s").unwrap()), "30");
    assert_eq!(val(rows[0].get("avg").unwrap()), "15");

    // cat b: single bound amount {5} — COUNT=1, SUM=AVG=5.
    assert_eq!(val(rows[1].get("cat").unwrap()), "http://ex/b");
    assert_eq!(val(rows[1].get("c").unwrap()), "1");
    assert_eq!(val(rows[1].get("s").unwrap()), "5");
    assert_eq!(val(rows[1].get("avg").unwrap()), "5");
}

#[test]
fn fast_count_distinct_matches_identity_dedup() {
    // Two entities in the group share the same ?v literal (same lexical
    // form -> same interned TermId): COUNT(DISTINCT ?v) must count it once.
    // SUM(?v) (not distinct) forces the query off the plain-count pushdown
    // and pins the non-distinct multiset alongside it.
    let mut st = HornBackend::new();
    for (e, v) in [("e1", "10.0"), ("e2", "10.0"), ("e3", "20.0")] {
        st.insert_triple(iri(e), iri("cat"), iri("a"));
        st.insert_triple(iri(e), iri("v"), dbl(v));
    }

    let rows = solutions(
        "SELECT ?cat (COUNT(DISTINCT ?v) AS ?cd) (SUM(?v) AS ?s) WHERE { \
         ?e <http://ex/cat> ?cat . ?e <http://ex/v> ?v } GROUP BY ?cat",
        &st,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        val(rows[0].get("cd").unwrap()),
        "2",
        "distinct values {{10.0, 20.0}}"
    );
    assert_eq!(
        val(rows[0].get("s").unwrap()),
        "40",
        "plain multiset 10+10+20"
    );
}

#[test]
fn fast_min_max_preserve_original_literal() {
    // All-numeric group: the fast MIN/MAX path must return the group
    // member's ORIGINAL term (exact lexical form, including datatype), not
    // a value recomputed from the decoded f64.
    let mut st = HornBackend::new();
    for (e, v) in [("e1", "3.5"), ("e2", "1.5"), ("e3", "2.5")] {
        st.insert_triple(iri(e), iri("cat"), iri("a"));
        st.insert_triple(iri(e), iri("v"), dbl(v));
    }
    let rows = solutions(
        "SELECT ?cat (MIN(?v) AS ?mn) (MAX(?v) AS ?mx) (SUM(?v) AS ?s) WHERE { \
         ?e <http://ex/cat> ?cat . ?e <http://ex/v> ?v } GROUP BY ?cat",
        &st,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        raw(rows[0].get("mn").unwrap()),
        "\"1.5\"^^<http://www.w3.org/2001/XMLSchema#double>",
        "MIN must be the exact original literal, not a recomputed one"
    );
    assert_eq!(
        raw(rows[0].get("mx").unwrap()),
        "\"3.5\"^^<http://www.w3.org/2001/XMLSchema#double>"
    );
}

#[test]
fn fast_min_max_falls_back_on_mixed_types_to_lexical_order() {
    // A non-numeric value in the group forces the fast MIN/MAX path off its
    // numeric-only fast lane and into the full-decode lexical-order
    // fallback — matching `aggregate_extreme`'s general-path rule exactly:
    // compare by the raw lexical (N-Triples-ish) string. `"3.5"^^<...>`
    // starts with `"3`, `"apple"` starts with `"a`; `'3' < 'a'` byte-wise, so
    // the double literal sorts first lexically despite being a smaller
    // string overall — MIN picks it, MAX picks the string.
    let mut st = HornBackend::new();
    st.insert_triple(iri("e1"), iri("cat"), iri("a"));
    st.insert_triple(iri("e1"), iri("v"), dbl("3.5"));
    st.insert_triple(iri("e2"), iri("cat"), iri("a"));
    st.insert_triple(iri("e2"), iri("v"), plain("apple"));

    let rows = solutions(
        "SELECT ?cat (MIN(?v) AS ?mn) (MAX(?v) AS ?mx) WHERE { \
         ?e <http://ex/cat> ?cat . ?e <http://ex/v> ?v } GROUP BY ?cat",
        &st,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        raw(rows[0].get("mn").unwrap()),
        "\"3.5\"^^<http://www.w3.org/2001/XMLSchema#double>",
        "lexical MIN of a mixed-type column"
    );
    assert_eq!(raw(rows[0].get("mx").unwrap()), "\"apple\"", "lexical MAX");
}

#[test]
fn scalar_key_grouping_handles_unbound_key_bucket() {
    // GROUP BY over a variable that is Unbound for some rows (OPTIONAL) and
    // Slot::Id for others, in the SAME column — the scalar "narrow" grouping
    // path (Do-3) must bucket Unbound separately, not crash trying to read
    // it as an id. SUM keeps the query off the count-only pushdown.
    let mut st = HornBackend::new();
    st.insert_triple(iri("e1"), iri("opt"), iri("g1"));
    st.insert_triple(iri("e1"), iri("v"), dbl("10.0"));
    st.insert_triple(iri("e2"), iri("opt"), iri("g1"));
    st.insert_triple(iri("e2"), iri("v"), dbl("20.0"));
    // e3 has no <opt> triple: ?g is Unbound for this row.
    st.insert_triple(iri("e3"), iri("v"), dbl("100.0"));

    let rows = solutions(
        "SELECT ?g (COUNT(?e) AS ?c) (SUM(?v) AS ?s) WHERE { \
         ?e <http://ex/v> ?v . \
         OPTIONAL { ?e <http://ex/opt> ?g } } GROUP BY ?g",
        &st,
    );
    assert_eq!(rows.len(), 2, "bound-?g group and the Unbound-?g group");

    let bound = rows
        .iter()
        .find(|r| r.get("g").is_some())
        .expect("one row has ?g bound");
    assert_eq!(val(bound.get("g").unwrap()), "http://ex/g1");
    assert_eq!(val(bound.get("c").unwrap()), "2");
    assert_eq!(val(bound.get("s").unwrap()), "30");

    let unbound = rows
        .iter()
        .find(|r| r.get("g").is_none())
        .expect("one row has ?g unbound");
    assert_eq!(val(unbound.get("c").unwrap()), "1");
    assert_eq!(val(unbound.get("s").unwrap()), "100");
}

#[test]
fn fast_and_general_aggregates_coexist_in_one_query() {
    // Two-key GROUP BY (the general Vec<KeyPart> grouping path, not the
    // scalar one) with a fast aggregate (SUM, bare column) and a general one
    // (GROUP_CONCAT, no fast path) side by side — proves the fast/general
    // split in `eval_group_native` doesn't starve the general aggregate of
    // the decode it still needs, and doesn't let the general aggregate force
    // extra decode onto the fast one's column.
    let mut st = HornBackend::new();
    for (e, cat, sub, v, label) in [
        ("e1", "a", "x", "10.0", "L1"),
        ("e2", "a", "x", "20.0", "L2"),
        ("e3", "a", "y", "5.0", "L3"),
    ] {
        st.insert_triple(iri(e), iri("cat"), iri(cat));
        st.insert_triple(iri(e), iri("sub"), iri(sub));
        st.insert_triple(iri(e), iri("v"), dbl(v));
        st.insert_triple(iri(e), iri("label"), plain(label));
    }

    let rows = solutions(
        "SELECT ?cat ?sub (SUM(?v) AS ?s) (GROUP_CONCAT(?label; SEPARATOR=\",\") AS ?labels) \
         WHERE { ?e <http://ex/cat> ?cat . ?e <http://ex/sub> ?sub . \
         ?e <http://ex/v> ?v . ?e <http://ex/label> ?label } \
         GROUP BY ?cat ?sub ORDER BY ?sub",
        &st,
    );
    assert_eq!(rows.len(), 2, "two (cat, sub) groups");

    // sub=x: {e1, e2} — SUM=30, labels L1,L2 (member order).
    assert_eq!(val(rows[0].get("sub").unwrap()), "http://ex/x");
    assert_eq!(val(rows[0].get("s").unwrap()), "30");
    assert_eq!(val(rows[0].get("labels").unwrap()), "L1,L2");

    // sub=y: {e3} — SUM=5, labels L3.
    assert_eq!(val(rows[1].get("sub").unwrap()), "http://ex/y");
    assert_eq!(val(rows[1].get("s").unwrap()), "5");
    assert_eq!(val(rows[1].get("labels").unwrap()), "L3");
}
