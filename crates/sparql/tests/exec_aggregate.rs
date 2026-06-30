//! GROUP BY + aggregate evaluation tests.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    // alice knows bob, carol; bob knows dave. 3 knows-triples.
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/knows"),
        iri("http://ex/bob"),
    );
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/knows"),
        iri("http://ex/carol"),
    );
    s.insert_triple(
        iri("http://ex/bob"),
        iri("http://ex/knows"),
        iri("http://ex/dave"),
    );
    s
}

fn solutions(q: &str, store: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    match execute_query(q, store).expect("query") {
        QueryAnswer::Solutions { rows, .. } => rows,
        other => panic!("expected solutions, got {other:?}"),
    }
}

/// Read the lexical value out of a bound literal/iri term.
fn val(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::BlankNode(s) => s.clone(),
        Term::Literal(raw) => {
            // strip the surrounding quotes / datatype for the simple
            // `"42"^^<...>` shape used by integer aggregates.
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

#[test]
fn implicit_group_count_star() {
    let s = make_store();
    let rows = solutions("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }", &s);
    assert_eq!(rows.len(), 1, "implicit group yields exactly one row");
    assert_eq!(val(rows[0].get("c").unwrap()), "3");
}

#[test]
fn implicit_group_count_star_zero_when_no_match() {
    let s = make_store();
    let rows = solutions(
        "SELECT (COUNT(*) AS ?c) WHERE { ?s <http://ex/nope> ?o }",
        &s,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(val(rows[0].get("c").unwrap()), "0");
}

#[test]
fn count_var() {
    let s = make_store();
    let rows = solutions(
        "SELECT (COUNT(?o) AS ?c) WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "3");
}

#[test]
fn count_distinct() {
    let s = make_store();
    // Distinct subjects: alice, bob -> 2.
    let rows = solutions(
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "2");
}

#[test]
fn group_by_subject_count() {
    let s = make_store();
    let rows = solutions(
        "SELECT ?s (COUNT(?o) AS ?c) WHERE { ?s <http://ex/knows> ?o } GROUP BY ?s",
        &s,
    );
    assert_eq!(rows.len(), 2, "two distinct subjects");
    let mut got: Vec<(String, String)> = rows
        .iter()
        .map(|r| (val(r.get("s").unwrap()), val(r.get("c").unwrap())))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("http://ex/alice".to_string(), "2".to_string()),
            ("http://ex/bob".to_string(), "1".to_string()),
        ]
    );
}

/// Store with deliberate duplicates, for the DISTINCT dedup paths (#128).
/// Six inserts over three subjects; MemStore dedups the two identical
/// triples, leaving four distinct rows with ?v multiset {10,20,30,30}.
fn dup_store() -> MemStore {
    let mut s = MemStore::default();
    let lit = |n: &str| {
        Term::Literal(format!(
            "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ))
    };
    for (subj, v) in [
        ("s1", "10"),
        ("s1", "10"),
        ("s2", "20"),
        ("s2", "30"),
        ("s3", "30"),
        ("s3", "30"),
    ] {
        s.insert_triple(
            iri(&format!("http://ex/{subj}")),
            iri("http://ex/v"),
            lit(v),
        );
    }
    s
}

#[test]
fn select_distinct_dedups_rows() {
    // Whole-row dedup path (PhysicalPlan::Distinct). MemStore's HashSet scan
    // order is non-deterministic, so assert the distinct SET, not an order.
    let s = dup_store();
    let rows = solutions("SELECT DISTINCT ?v WHERE { ?s <http://ex/v> ?v }", &s);
    let mut got: Vec<String> = rows.iter().map(|r| val(r.get("v").unwrap())).collect();
    got.sort();
    assert_eq!(got, vec!["10", "20", "30"], "three distinct ?v values");
}

#[test]
fn sum_distinct_dedups_before_summing() {
    // MemStore dedups identical triples, so the stored ?v multiset over the
    // four distinct rows is {10,20,30,30}; DISTINCT collapses it to {10,20,30}.
    let s = dup_store();
    let rows = solutions(
        "SELECT (SUM(DISTINCT ?v) AS ?t) WHERE { ?s <http://ex/v> ?v }",
        &s,
    );
    assert_eq!(
        val(rows[0].get("t").unwrap()),
        "60",
        "SUM(DISTINCT) = 10+20+30"
    );
    // Plain SUM sees the full stored multiset: 10+20+30+30 = 90.
    let rows = solutions("SELECT (SUM(?v) AS ?t) WHERE { ?s <http://ex/v> ?v }", &s);
    assert_eq!(val(rows[0].get("t").unwrap()), "90", "SUM = 10+20+30+30");
}

#[test]
fn count_distinct_term_and_star() {
    let s = dup_store();
    // COUNT(DISTINCT ?v) — dedup_terms path: distinct values {10,20,30} = 3.
    let rows = solutions(
        "SELECT (COUNT(DISTINCT ?v) AS ?c) WHERE { ?s <http://ex/v> ?v }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "3");
    // COUNT(DISTINCT *) — whole-row dedup path: the four (?s,?v) solutions are
    // all distinct (?s distinguishes them), so the count is 4 = COUNT(*).
    let rows = solutions(
        "SELECT (COUNT(DISTINCT *) AS ?c) WHERE { ?s <http://ex/v> ?v }",
        &s,
    );
    assert_eq!(val(rows[0].get("c").unwrap()), "4");
}

/// Three entities over two categories: cat A has {e1, e2}, cat B has {e3}.
fn cat_store() -> MemStore {
    let mut s = MemStore::default();
    for (e, c) in [("e1", "A"), ("e2", "A"), ("e3", "B")] {
        s.insert_triple(
            iri(&format!("http://ex/{e}")),
            iri("http://ex/cat"),
            iri(&format!("http://ex/{c}")),
        );
    }
    s
}

/// Targeted `GROUP BY` + `COUNT(DISTINCT *)` (#145, increment 5 of #128).
///
/// The differential proptest exercises this only indirectly; this case pins the
/// per-group id-based distinct path directly (`eval_group_native`, the
/// `Vec<KeyPart>` `HashSet` over a group's member slot rows). A self-`UNION`
/// duplicates every solution as a multiset (`PhysicalPlan::Union` is
/// `rows.extend`, no dedup), so within each group `COUNT(DISTINCT *)` must
/// collapse the duplicate `(?e, ?c)` rows back down while plain `COUNT(*)` sees
/// the doubled multiset. Asserting both per group proves the distinct keying is
/// scoped to the group and actually dedups — not just a member tally.
#[test]
fn group_by_count_distinct_star() {
    let s = cat_store();
    let rows = solutions(
        "SELECT ?c (COUNT(*) AS ?all) (COUNT(DISTINCT *) AS ?dist) \
         WHERE { { ?e <http://ex/cat> ?c } UNION { ?e <http://ex/cat> ?c } } \
         GROUP BY ?c",
        &s,
    );

    // Two groups; MemStore's scan order is non-deterministic, so key by ?c.
    use std::collections::HashMap;
    let got: HashMap<String, (String, String)> = rows
        .iter()
        .map(|r| {
            (
                val(r.get("c").unwrap()),
                (val(r.get("all").unwrap()), val(r.get("dist").unwrap())),
            )
        })
        .collect();
    assert_eq!(got.len(), 2, "one row per category");

    // cat A: {e1, e2} doubled by UNION → COUNT(*) = 4, distinct (?e,?c) = 2.
    assert_eq!(
        got.get("http://ex/A"),
        Some(&("4".to_owned(), "2".to_owned())),
        "cat A: COUNT(*)=4, COUNT(DISTINCT *)=2"
    );
    // cat B: {e3} doubled by UNION → COUNT(*) = 2, distinct (?e,?c) = 1.
    assert_eq!(
        got.get("http://ex/B"),
        Some(&("2".to_owned(), "1".to_owned())),
        "cat B: COUNT(*)=2, COUNT(DISTINCT *)=1"
    );
}
