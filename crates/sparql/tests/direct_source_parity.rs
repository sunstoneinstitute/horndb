//! HDB-120 parity: `HornBackend` reading the columnar partitions directly
//! must answer exactly what it answers from the `VecTripleSource` copy.
//!
//! `VecTripleSource` is the oracle here — the path every query took before
//! HDB-120 — so the two backends differ only in
//! [`HornBackend::set_direct_source`]. Same fixture, same queries, results
//! compared as multisets (an unordered SPARQL result set has no row order to
//! preserve, so every case that could differ carries `ORDER BY`).

use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;

const P: &str = "http://ex/p";
const Q: &str = "http://ex/q";
const G1: &str = "http://ex/g1";
const G2: &str = "http://ex/g2";

fn iri(v: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v))
}

/// A backend with a small many-predicate graph plus two named graphs.
///
/// Several predicates so the merge has several leaves; a chain and a cycle so
/// multi-pattern joins bind at every trie depth; a shared object so the
/// object-major axis has real fan-out; and named graphs so both the
/// single-graph direct path and the multi-graph union fallback are covered.
fn fixture(direct: bool) -> HornBackend {
    fixture_tiered(direct, false)
}

/// [`fixture`], plus (when `cold`) a `demote_all` pass that pushes every
/// settled partition into the memory-mapped cold tier (SPEC-25 S5). The
/// backend is otherwise identical, so any answer that differs is a cold-path
/// bug — this is what pins `PredicatePartition::ordered_at` over a
/// `ColdPartition` in a unit test rather than only in the conformance
/// harness.
fn fixture_tiered(direct: bool, cold: bool) -> HornBackend {
    let mut b = HornBackend::new();
    b.set_direct_source(direct);
    for i in 0..24u64 {
        b.insert_oxrdf(
            &iri(&format!("http://ex/s{i}")),
            &iri(P),
            &iri(&format!("http://ex/s{}", (i + 1) % 24)),
        )
        .unwrap();
        b.insert_oxrdf(
            &iri(&format!("http://ex/s{i}")),
            &iri(Q),
            &iri(&format!("http://ex/hub{}", i % 3)),
        )
        .unwrap();
        b.insert_oxrdf(
            &iri(&format!("http://ex/s{i}")),
            &iri("http://ex/r"),
            &oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(format!("lit{}", i % 5))),
        )
        .unwrap();
    }
    b.insert_oxrdf_in_named_graph(&iri(G1), &iri("http://ex/a"), &iri(P), &iri("http://ex/b"))
        .unwrap();
    b.insert_oxrdf_in_named_graph(&iri(G2), &iri("http://ex/a"), &iri(P), &iri("http://ex/b"))
        .unwrap();
    b.insert_oxrdf_in_named_graph(&iri(G2), &iri("http://ex/c"), &iri(Q), &iri("http://ex/d"))
        .unwrap();
    if cold {
        assert!(b.demote_all().unwrap() > 0, "nothing was demoted");
    }
    b
}

/// A query answer as a comparable multiset of lines.
fn answer(b: &HornBackend, q: &str) -> Vec<String> {
    let mut out = match execute_query(q, b).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryAnswer::Solutions { vars, rows } => {
            let mut v: Vec<String> = rows
                .iter()
                .map(|r| {
                    vars.iter()
                        .map(|name| format!("{name}={:?}", r.get(name)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            v.push(format!("vars={vars:?}"));
            v
        }
        QueryAnswer::Boolean(b) => vec![format!("ask={b}")],
        QueryAnswer::Triples(t) => t.iter().map(|x| format!("{x:?}")).collect(),
        QueryAnswer::Explanation { text, .. } => vec![text],
    };
    out.sort();
    out
}

const QUERIES: &[&str] = &[
    // Whole-store scan, every predicate — the unbound-predicate shape, which
    // makes every leaf live at trie depth 0.
    "SELECT ?s ?p ?o WHERE { ?s ?p ?o }",
    // Bound predicate: one leaf.
    "SELECT ?s ?o WHERE { ?s <http://ex/p> ?o }",
    // Bound object: drives the object-major axis.
    "SELECT ?s WHERE { ?s <http://ex/q> <http://ex/hub1> }",
    // Bound subject across predicates: depth 0 is the subject, depth 1 the
    // predicate key.
    "SELECT ?p ?o WHERE { <http://ex/s3> ?p ?o }",
    // Two-pattern join over one predicate (chain step).
    "SELECT ?a ?b ?c WHERE { ?a <http://ex/p> ?b . ?b <http://ex/p> ?c }",
    // Three-pattern join mixing predicates — the WCOJ path proper.
    "SELECT ?a ?b ?h WHERE { ?a <http://ex/p> ?b . ?b <http://ex/q> ?h . ?a <http://ex/q> ?h }",
    // Join through a literal object.
    "SELECT ?a ?b WHERE { ?a <http://ex/r> ?l . ?b <http://ex/r> ?l . ?a <http://ex/p> ?b }",
    // Fully ground: the membership test, both a hit and a miss.
    "ASK { <http://ex/s0> <http://ex/p> <http://ex/s1> }",
    "ASK { <http://ex/s0> <http://ex/p> <http://ex/s7> }",
    // Aggregation and DISTINCT over a scan.
    "SELECT ?h (COUNT(*) AS ?n) WHERE { ?s <http://ex/q> ?h } GROUP BY ?h ORDER BY ?h",
    "SELECT DISTINCT ?l WHERE { ?s <http://ex/r> ?l } ORDER BY ?l",
    // OPTIONAL and FILTER.
    "SELECT ?s ?o WHERE { ?s <http://ex/p> ?o OPTIONAL { ?o <http://ex/q> ?h } }",
    "SELECT ?s WHERE { ?s <http://ex/q> ?h FILTER(?h = <http://ex/hub0>) }",
    // Property path over the cycle.
    "SELECT ?b WHERE { <http://ex/s0> <http://ex/p>+ ?b } ORDER BY ?b",
    // CONSTRUCT re-materialises rows through the same scan.
    "CONSTRUCT { ?s <http://ex/p> ?o } WHERE { ?s <http://ex/p> ?o }",
    // Named graphs: a ground single-graph scope (direct), and the two forms
    // that must fall back to the copy — a two-graph `FROM` union and the
    // per-graph loop.
    "SELECT ?s ?o WHERE { GRAPH <http://ex/g2> { ?s <http://ex/p> ?o } }",
    "SELECT ?s ?p ?o FROM <http://ex/g1> FROM <http://ex/g2> WHERE { ?s ?p ?o }",
    "SELECT ?g ?s WHERE { GRAPH ?g { ?s <http://ex/p> ?o } } ORDER BY ?g ?s",
    // An unknown graph is zero rows, not an error.
    "SELECT ?s WHERE { GRAPH <http://ex/nope> { ?s ?p ?o } }",
];

#[test]
fn direct_source_answers_match_the_vec_source_oracle() {
    let direct = fixture(true);
    let oracle = fixture(false);
    for q in QUERIES {
        assert_eq!(answer(&direct, q), answer(&oracle, q), "query: {q}");
    }
}

#[test]
fn parity_survives_writes_and_retractions() {
    let mut direct = fixture(true);
    let mut oracle = fixture(false);
    // A small insert (the delta-merge path for the copy), then a retraction
    // (which forces the partition's visibility-filtered read path), then a
    // re-insert of a retracted triple.
    for update in [
        "INSERT DATA { <http://ex/s0> <http://ex/p> <http://ex/new> }",
        "DELETE DATA { <http://ex/s1> <http://ex/p> <http://ex/s2> }",
        "DELETE WHERE { ?s <http://ex/r> \"lit0\" }",
        "INSERT DATA { <http://ex/s1> <http://ex/p> <http://ex/s2> }",
    ] {
        execute_update(update, &mut direct).unwrap();
        execute_update(update, &mut oracle).unwrap();
        for q in QUERIES {
            assert_eq!(
                answer(&direct, q),
                answer(&oracle, q),
                "after `{update}`, query: {q}"
            );
        }
    }
}

/// A pinned read view (HDB-119) reading through the direct source answers at
/// *its* commit version, not the store's latest.
///
/// The two features meet in `HornBackend::direct_source_for`: it opens the
/// source over `snap()` (the pinned tier, not `store.snapshot()`), and the
/// one-entry `direct_cache` is shared with every view, keyed by the version it
/// was opened at. Both halves are load-bearing — a source opened over the
/// latest tier, or a cache hit across versions, would let a pinned view see a
/// write committed after it was pinned.
///
/// Scoped to a named graph on purpose: this fixture has three live graphs, so
/// a default-graph query resolves to `DefaultUnion` over more than one graph
/// and takes the `VecTripleSource` fallback. `GRAPH <g2>` resolves to
/// `OneGraph`, which is what actually reaches the direct source.
#[test]
fn a_pinned_view_reads_its_own_version_through_the_direct_source() {
    const Q: &str =
        "SELECT ?s ?o WHERE { GRAPH <http://ex/g2> { ?s <http://ex/p> ?o } } ORDER BY ?s ?o";
    let mut direct = fixture(true);
    let mut oracle = fixture(false);

    let (pinned_direct, pinned_oracle) = (direct.pin_read(), oracle.pin_read());
    let (before_direct, before_oracle) = (answer(&pinned_direct, Q), answer(&pinned_oracle, Q));
    assert_eq!(before_direct, before_oracle);

    let update =
        "INSERT DATA { GRAPH <http://ex/g2> { <http://ex/a2> <http://ex/p> <http://ex/b2> } }";
    execute_update(update, &mut direct).unwrap();
    execute_update(update, &mut oracle).unwrap();

    // The write is invisible to the views pinned before it, on both paths...
    assert_eq!(
        answer(&pinned_direct, Q),
        before_direct,
        "pinned view drifted"
    );
    assert_eq!(answer(&pinned_direct, Q), answer(&pinned_oracle, Q));
    // ...and visible to the backends themselves, which read the latest state.
    // (Asked after the pinned reads, so a stale `direct_cache` entry would
    // have to be rejected on its version tag for this to hold.)
    assert_eq!(answer(&direct, Q), answer(&oracle, Q));
    assert_ne!(answer(&direct, Q), before_direct, "write never landed");
}

/// SPEC-25 S5 acceptance #5, unit-test half: the same oracle comparison with
/// every partition demoted to the cold tier. Both read paths are covered —
/// the `VecTripleSource` copy (which decodes a cold partition once via
/// `scan_at`) and the direct source (which reads `ordered_at` straight off
/// `ColdPartition`).
#[test]
fn cold_partitions_answer_what_warm_ones_do() {
    let oracle = fixture_tiered(false, false);
    for (direct, cold) in [(false, true), (true, false), (true, true)] {
        let b = fixture_tiered(direct, cold);
        for q in QUERIES {
            assert_eq!(
                answer(&b, q),
                answer(&oracle, q),
                "direct={direct} cold={cold}, query: {q}"
            );
        }
    }
}

/// A write to a cold partition promotes it, applies, and (here, by hand)
/// re-demotes — the promote/demote round-trip the `HORNDB_COLD_TIER` harness
/// run puts every update case through.
#[test]
fn cold_parity_survives_writes_and_retractions() {
    let mut cold = fixture_tiered(true, true);
    let mut oracle = fixture_tiered(false, false);
    for update in [
        "INSERT DATA { <http://ex/s0> <http://ex/p> <http://ex/new> }",
        "DELETE DATA { <http://ex/s1> <http://ex/p> <http://ex/s2> }",
        "DELETE WHERE { ?s <http://ex/r> \"lit0\" }",
        "INSERT DATA { <http://ex/s1> <http://ex/p> <http://ex/s2> }",
    ] {
        execute_update(update, &mut cold).unwrap();
        execute_update(update, &mut oracle).unwrap();
        cold.demote_all().unwrap();
        for q in QUERIES {
            assert_eq!(
                answer(&cold, q),
                answer(&oracle, q),
                "after `{update}` on a cold store, query: {q}"
            );
        }
    }
}
