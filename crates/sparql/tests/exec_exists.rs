//! `EXISTS` / `NOT EXISTS` as a FILTER expression (SPARQL 1.1 §18.6, HDB-134).
//!
//! The W3C `exists/` and `negation/` cases cover the basics end to end, but
//! they need the fetched corpus and they under-specify the *correlated*
//! behaviour. These run in the plain `cargo nextest` pass and pin the three
//! things §18.6 turns on: that the pattern sees the current row's bindings,
//! that a variable the row does not bind stays free, and that `EXISTS` works
//! in a value position (`BIND`, `IF`) and not only as a whole `FILTER`.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

const EX: &str = "http://example.org/";

fn iri(local: &str) -> Term {
    Term::Iri(format!("{EX}{local}"))
}

/// `:a` and `:b` are both `:Thing`; only `:a` has a `:flag`. `:a :p :x` and
/// `:b :p :y` give every subject one `:p` value.
fn store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("a"), iri("type"), iri("Thing"));
    s.insert_triple(iri("b"), iri("type"), iri("Thing"));
    s.insert_triple(iri("a"), iri("flag"), iri("on"));
    s.insert_triple(iri("a"), iri("p"), iri("x"));
    s.insert_triple(iri("b"), iri("p"), iri("y"));
    s
}

fn rows(q: &str) -> Vec<horndb_sparql::exec::Bindings> {
    let answer = execute_query(q, &store()).expect("query should run");
    let QueryAnswer::Solutions { rows, .. } = answer else {
        panic!("expected solutions");
    };
    rows
}

/// The local names bound to `?s`, sorted.
fn subjects(q: &str) -> Vec<String> {
    let mut out: Vec<String> = rows(q)
        .iter()
        .map(|b| match b.get("s").expect("?s bound") {
            Term::Iri(s) => s.trim_start_matches(EX).to_owned(),
            other => panic!("unexpected term {other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// The whole point of §18.6: the pattern is evaluated per row with that
/// row's `?s` substituted in, so it separates `:a` from `:b`. A one-shot
/// evaluation hoisted out of the row loop would keep both (the pattern has a
/// solution) or neither.
#[test]
fn exists_is_correlated_with_the_current_row() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing \
         FILTER EXISTS {{ ?s :flag :on }} }}"
    );
    assert_eq!(subjects(&q), vec!["a"]);
}

#[test]
fn not_exists_is_the_per_row_negation() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing \
         FILTER NOT EXISTS {{ ?s :flag :on }} }}"
    );
    assert_eq!(subjects(&q), vec!["b"]);
}

/// `EXISTS` in a value position, not as the whole `FILTER` — the gap HDB-134
/// closed. The `BIND` must see the current row's `?s`, so `?has` differs
/// between the two rows.
#[test]
fn exists_inside_bind_sees_the_current_row() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s ?has WHERE {{ ?s :type :Thing \
         BIND(EXISTS {{ ?s :flag :on }} AS ?has) }}"
    );
    let mut got: Vec<(String, String)> = rows(&q)
        .iter()
        .map(|b| {
            let Term::Iri(s) = b.get("s").expect("?s bound") else {
                panic!("?s should be an IRI");
            };
            let Term::Literal(h) = b.get("has").expect("?has bound") else {
                panic!("?has should be a literal");
            };
            (s.trim_start_matches(EX).to_owned(), h.clone())
        })
        .collect();
    got.sort();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].0 == "a" && got[0].1.starts_with("\"true\""),
        "{got:?}"
    );
    assert!(
        got[1].0 == "b" && got[1].1.starts_with("\"false\""),
        "{got:?}"
    );
}

/// `EXISTS` combined with `&&` / `||` inside a `FILTER` — the other shape
/// that used to be refused.
#[test]
fn exists_combines_with_boolean_connectives() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing \
         FILTER(true && NOT EXISTS {{ ?s :flag :on }}) }}"
    );
    assert_eq!(subjects(&q), vec!["b"]);
}

/// A variable that occurs only inside the `EXISTS` pattern is not in
/// `dom(μ)`, so §18.6 leaves it free: it ranges over the data rather than
/// being correlated with the outer row. Here `?o` is never bound outside,
/// so the pattern asks "does `?s` have *any* `:p` value" — true for both
/// subjects. If the inner `?o` were wrongly treated as an outer variable
/// forced to be unbound, neither row would survive.
#[test]
fn inner_only_variable_stays_free() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing \
         FILTER EXISTS {{ ?s :p ?o }} }}"
    );
    assert_eq!(subjects(&q), vec!["a", "b"]);
}

/// The same variable name *is* correlated once the outer row binds it: `?o`
/// bound to `:x` by the outer pattern makes the inner `?s :p ?o` a ground
/// test, which only `:a` passes. This is the counterpart to the test above —
/// together they pin that correlation follows `dom(μ)`, not the name.
#[test]
fn a_bound_variable_of_the_same_name_is_correlated() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing . :a :p ?o \
         FILTER EXISTS {{ ?s :p ?o }} }}"
    );
    assert_eq!(subjects(&q), vec!["a"]);
}

/// Nested `EXISTS` inside `EXISTS` (W3C `exists04`/`exists05`): the outer
/// row's substitution has to reach the inner pattern too.
#[test]
fn nested_exists_substitutes_through_both_levels() {
    let q = format!(
        "PREFIX : <{EX}> SELECT ?s WHERE {{ ?s :type :Thing \
         FILTER EXISTS {{ ?s :p ?o FILTER NOT EXISTS {{ ?s :flag :on }} }} }}"
    );
    assert_eq!(subjects(&q), vec!["b"]);
}
