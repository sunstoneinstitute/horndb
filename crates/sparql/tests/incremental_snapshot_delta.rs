//! Behavioral tests for the delta-maintained WCOJ snapshot
//! (PLAN-03-03-incremental-snapshot-delta.md, Task 2, HDB-82).
//!
//! `HornBackend::apply_delta_to_snapshots` merges a small `SPARQL Update`
//! into the cached snapshot in place instead of dropping it, so these tests
//! hold it to the plan's bar: behaviour identical to a full rebuild, the
//! merge actually landing (not silently doing nothing), and multi-graph
//! union correctness when the fast path must bail.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use std::collections::HashSet;

fn seed(triples: &[(&str, &str, &str)]) -> HornBackend {
    let mut b = HornBackend::new();
    for (s, p, o) in triples {
        b.insert_triple(
            Term::Iri((*s).to_owned()),
            Term::Iri((*p).to_owned()),
            Term::Iri((*o).to_owned()),
        );
    }
    b
}

fn apply(store: &mut HornBackend, update: &str) {
    apply_update(&parse_update(update).unwrap(), store).unwrap();
}

fn select(store: &HornBackend, query: &str) -> Vec<horndb_sparql::exec::Bindings> {
    let QueryAnswer::Solutions { rows, .. } = execute_query(query, store).unwrap() else {
        panic!("expected solutions for {query}");
    };
    rows
}

/// Every `(?s, ?p, ?o)` solution over the default-union scope -- no `FROM`
/// clause means SPARQL's own default (`DefaultGraphMode::Union`), which folds
/// in every named graph too (SPEC-28 D2/S3). The yardstick for "this backend
/// holds the same triples as that one", independent of row order.
fn all_spo(store: &HornBackend) -> HashSet<(Term, Term, Term)> {
    select(store, "SELECT ?s ?p ?o WHERE { ?s ?p ?o }")
        .iter()
        .map(|r| {
            (
                r.get("s").unwrap().clone(),
                r.get("p").unwrap().clone(),
                r.get("o").unwrap().clone(),
            )
        })
        .collect()
}

// --- update_then_query_matches_fresh_backend ---------------------------
//
// Behaviour after a delta-merged update must be indistinguishable from a
// second backend built directly from the post-update triple set. One test
// per update shape the plan lists.

#[test]
fn update_then_query_matches_fresh_backend_insert_data() {
    let mut store = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    // Warm the memoised snapshot before mutating, so the update below runs
    // against a live cache and exercises `apply_delta_to_snapshots`'s merge
    // path instead of hitting its empty-cache early return.
    assert_eq!(all_spo(&store).len(), 1, "warm-up query");
    apply(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/c> }",
    );
    let fresh = seed(&[
        ("http://ex/a", "http://ex/p", "http://ex/b"),
        ("http://ex/a", "http://ex/p", "http://ex/c"),
    ]);
    assert_eq!(all_spo(&store), all_spo(&fresh));
}

#[test]
fn update_then_query_matches_fresh_backend_delete_data() {
    let mut store = seed(&[
        ("http://ex/a", "http://ex/p", "http://ex/b"),
        ("http://ex/a", "http://ex/p", "http://ex/c"),
    ]);
    // Warm the memoised snapshot before mutating -- see the insert_data test.
    assert_eq!(all_spo(&store).len(), 2, "warm-up query");
    apply(
        &mut store,
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
    );
    let fresh = seed(&[("http://ex/a", "http://ex/p", "http://ex/c")]);
    assert_eq!(all_spo(&store), all_spo(&fresh));
}

#[test]
fn update_then_query_matches_fresh_backend_delete_insert_where() {
    let mut store = seed(&[
        ("http://ex/a", "http://ex/old", "http://ex/b"),
        ("http://ex/a", "http://ex/keep", "http://ex/d"),
    ]);
    apply(
        &mut store,
        "DELETE { ?s <http://ex/old> ?o } INSERT { ?s <http://ex/new> ?o } \
         WHERE { ?s <http://ex/old> ?o }",
    );
    let fresh = seed(&[
        ("http://ex/a", "http://ex/new", "http://ex/b"),
        ("http://ex/a", "http://ex/keep", "http://ex/d"),
    ]);
    assert_eq!(all_spo(&store), all_spo(&fresh));
}

#[test]
fn update_then_query_matches_fresh_backend_delete_absent_triple() {
    let mut store = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    // Warm the memoised snapshot before mutating -- see the insert_data test.
    assert_eq!(all_spo(&store).len(), 1, "warm-up query");
    // The store never held this triple: the delete is a no-op, not an error,
    // and must not leave the snapshot in a wrong state.
    apply(
        &mut store,
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/nope> }",
    );
    let fresh = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    assert_eq!(all_spo(&store), all_spo(&fresh));
}

// --- visibility -----------------------------------------------------------

#[test]
fn mutation_is_visible_to_the_very_next_query() {
    let mut store = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    // Warm the memoised snapshot BEFORE mutating, so a merge that silently
    // does nothing (neither updating the cached snapshot nor invalidating
    // it) cannot pass by accident: the very next scan reads a snapshot that
    // already existed at insert time.
    assert_eq!(all_spo(&store).len(), 1, "warm-up query");

    apply(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/c> }",
    );

    let got = all_spo(&store);
    assert!(
        got.contains(&(
            Term::Iri("http://ex/a".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/c".into()),
        )),
        "the insert must be visible on the very next query: {got:?}"
    );
    assert_eq!(
        got.len(),
        2,
        "both the seed triple and the insert are visible"
    );
}

// --- multi-graph union fallback --------------------------------------------

#[test]
fn multi_graph_union_fallback_keeps_correctness_after_partial_delete() {
    let mut store = HornBackend::new();
    apply(
        &mut store,
        "INSERT DATA { GRAPH <http://ex/g1> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
    );
    apply(
        &mut store,
        "INSERT DATA { GRAPH <http://ex/g2> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
    );
    // Warm the union snapshot before the partial delete -- the same shape as
    // the correctness hazard this test guards against.
    assert_eq!(
        all_spo(&store).len(),
        1,
        "the union default graph dedupes the shared triple (SPEC-28 S3)"
    );

    apply(
        &mut store,
        "DELETE DATA { GRAPH <http://ex/g1> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
    );

    // g2 still holds the triple: a delta confined to g1's row must not drop
    // the union row while g2 is still live. This is the scope
    // `apply_delta_to_snapshots` must refuse to merge and fall back on.
    assert_eq!(
        all_spo(&store).len(),
        1,
        "the triple survives in the union default graph via g2"
    );
    assert!(
        select(
            &store,
            "SELECT ?s WHERE { GRAPH <http://ex/g1> { ?s <http://ex/p> <http://ex/o> } }",
        )
        .is_empty(),
        "g1 no longer holds the triple"
    );
    assert_eq!(
        select(
            &store,
            "SELECT ?s WHERE { GRAPH <http://ex/g2> { ?s <http://ex/p> <http://ex/o> } }",
        )
        .len(),
        1,
        "g2 is untouched"
    );
}

// --- EXPLAIN cardinality estimate freshness --------------------------------

#[test]
fn explain_cardinality_estimate_reflects_mutation_not_stats_cache() {
    let mut store = HornBackend::new();
    for i in 0..4 {
        store.insert_triple(
            Term::Iri(format!("http://ex/s{i}")),
            Term::Iri("http://ex/p".to_owned()),
            Term::Iri(format!("http://ex/o{i}")),
        );
    }
    let q = "EXPLAIN SELECT ?s ?o WHERE { ?s <http://ex/p> ?o }";

    let QueryAnswer::Explanation { text: before, .. } = execute_query(q, &store).unwrap() else {
        panic!("expected an Explanation")
    };
    assert!(before.contains("~4 rows"), "{before}");

    // A small delta -- one more matching triple, well under the fast path's
    // rebuild-instead threshold -- must not leave `stats_cache` describing the
    // pre-mutation snapshot. An in-place merge keeps the same snapshot `Arc`,
    // so pointer identity could never have told a stale entry from a fresh
    // one; the commit-version tag plus the merged summary (HDB-123) is what
    // does.
    apply(
        &mut store,
        "INSERT DATA { <http://ex/s4> <http://ex/p> <http://ex/o4> }",
    );

    let QueryAnswer::Explanation { text: after, .. } = execute_query(q, &store).unwrap() else {
        panic!("expected an Explanation")
    };
    assert!(
        !after.contains("~4 rows"),
        "stale cardinality estimate survived the mutation: {after}"
    );
    assert!(after.contains("~5 rows"), "{after}");
}
