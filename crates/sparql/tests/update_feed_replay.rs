//! SPEC-28 S7 acceptance criterion 7, at the **SPARQL layer**: the rendered
//! analog of `crates/storage/tests/feed_replay.rs`.
//!
//! The storage test drives `Store::apply_quads` directly. This one renders the
//! same kind of quad-grain change feed as SPARQL Update text — one
//! `DELETE DATA { … } ; INSERT DATA { … }` request per batch — and drives it
//! through the real update entry point (`parse_update` + `apply_update`, the
//! path `tests/update_named_graph.rs` uses). It pins two things:
//!
//! 1. **At-least-once replay converges.** A feed applied once cleanly and the
//!    same feed applied with duplicated deliveries (an immediate echo of every
//!    request, plus a stale-checkpoint replay of the tail from a random point)
//!    reach the *same* final quad set. This holds because each `DELETE DATA` /
//!    `INSERT DATA` operation is the idempotent set map `T(S) = (S \ D) ∪ A`,
//!    and a contiguous run of such maps composes to another idempotent map
//!    (`SPEC-28 S6`) — so redelivering any suffix cannot change the result.
//!    A single request `DELETE DATA{q};INSERT DATA{q}` renders one such feed
//!    element with `D = A = {q}` (the "del+add of the same quad" case), and
//!    because each operation is its own batch the quad ends **present**.
//!
//! 2. **One batch per operation, in request order.** A single request
//!    `DELETE DATA{q};INSERT DATA{q};DELETE DATA{q}` ends with `q` **absent**
//!    (the later delete wins over the earlier insert), and the mirror
//!    `INSERT;DELETE;INSERT` ends **present** — the operations are never
//!    collapsed into a net delta.
//!
//! Plus the non-canonical-literal identity pin: inserting `"01"^^xsd:integer`
//! and then deleting `"1"^^xsd:integer` leaves the `"01"` quad intact (quad
//! identity is lexical term equality — no value normalization).
//!
//! Both Stage-1 backends are exercised on every generated case.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use proptest::prelude::*;
use spargebra::algebra::GraphTarget;
use spargebra::term::NamedNode;
use std::collections::HashSet;

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const G1: &str = "http://ex/g1";

fn apply<B: FullBackend>(store: &mut B, request: &str) {
    let parsed = parse_update(request).unwrap_or_else(|e| panic!("parse `{request}`: {e}"));
    apply_update(&parsed, store).unwrap_or_else(|e| panic!("apply `{request}`: {e}"));
}

/// Every visible quad, decoded to terms and keyed by graph — the same D11 view
/// the W3C update suite compares.
fn dump<B: FullBackend>(store: &B) -> HashSet<(Option<String>, Term, Term, Term)> {
    let mut out = HashSet::new();
    for (s, p, o) in store.scan_graph_quads(&GraphTarget::DefaultGraph).unwrap() {
        out.insert((None, s, p, o));
    }
    for g in store.graphs() {
        let target = GraphTarget::NamedNode(NamedNode::new_unchecked(&g));
        for (s, p, o) in store.scan_graph_quads(&target).unwrap() {
            out.insert((Some(g.clone()), s, p, o));
        }
    }
    out
}

// ── The term space and feed ──────────────────────────────────────────────────
//
// 2 graphs (default + g1) x 3 subjects x 2 predicates x 3 objects = 36 quads,
// fed as 2-8 batches of 1-3 actions: small enough that the same quad is
// targeted by different batches often (so a replayed tail really does re-touch
// quads a later batch also touches — the interesting case).

#[derive(Clone, Copy, Debug)]
enum GraphSel {
    Default,
    Named,
}

/// (graph, subject index, predicate index, object index).
type QuadIdx = (GraphSel, usize, usize, usize);

fn arb_quad_idx() -> impl Strategy<Value = QuadIdx> {
    (
        prop_oneof![Just(GraphSel::Default), Just(GraphSel::Named)],
        0..3usize,
        0..2usize,
        0..3usize,
    )
}

#[derive(Clone, Debug)]
enum Action {
    Add(QuadIdx),
    Del(QuadIdx),
    /// Del-then-add of the SAME quad within one batch — acceptance-7's explicit
    /// "del+add of the same quad in one batch" requirement. Rendered as that
    /// quad appearing in both the `DELETE DATA` and the `INSERT DATA` half.
    Both(QuadIdx),
}

fn arb_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        arb_quad_idx().prop_map(Action::Add),
        arb_quad_idx().prop_map(Action::Del),
        arb_quad_idx().prop_map(Action::Both),
    ]
}

fn arb_feed() -> impl Strategy<Value = Vec<Vec<Action>>> {
    proptest::collection::vec(proptest::collection::vec(arb_action(), 1..=3), 2..=8)
}

/// A rendered triple `<s> <p> <o> .` for the small IRI term space.
fn triple(idx: QuadIdx) -> (GraphSel, String) {
    let (g, s, p, o) = idx;
    (
        g,
        format!("<http://ex/s{s}> <http://ex/p{p}> <http://ex/o{o}> ."),
    )
}

/// Group a batch's del / add quads and render the request `DELETE DATA { … } ;
/// INSERT DATA { … }` (each half omitted when empty). Returns `None` for an
/// all-empty batch (nothing to send).
fn render_batch(actions: &[Action]) -> Option<String> {
    let mut dels: Vec<(GraphSel, String)> = Vec::new();
    let mut adds: Vec<(GraphSel, String)> = Vec::new();
    for a in actions {
        match a {
            Action::Add(q) => adds.push(triple(*q)),
            Action::Del(q) => dels.push(triple(*q)),
            Action::Both(q) => {
                dels.push(triple(*q));
                adds.push(triple(*q));
            }
        }
    }
    let mut ops: Vec<String> = Vec::new();
    if !dels.is_empty() {
        ops.push(format!("DELETE DATA {{ {} }}", render_block(&dels)));
    }
    if !adds.is_empty() {
        ops.push(format!("INSERT DATA {{ {} }}", render_block(&adds)));
    }
    (!ops.is_empty()).then(|| ops.join(" ; "))
}

/// Render a set of `(graph, triple)` lines into a DATA block body: default-graph
/// triples at top level, named-graph triples inside `GRAPH <g1> { … }`.
/// Duplicate lines are collapsed (a DATA block is a set).
fn render_block(lines: &[(GraphSel, String)]) -> String {
    let mut default: Vec<&str> = Vec::new();
    let mut named: Vec<&str> = Vec::new();
    for (g, line) in lines {
        let bucket = match g {
            GraphSel::Default => &mut default,
            GraphSel::Named => &mut named,
        };
        if !bucket.contains(&line.as_str()) {
            bucket.push(line);
        }
    }
    let mut body = default.join(" ");
    if !named.is_empty() {
        body.push_str(&format!(" GRAPH <{G1}> {{ {} }}", named.join(" ")));
    }
    body
}

/// Turn a feed into one request string per non-empty batch.
fn render_feed(feed: &[Vec<Action>]) -> Vec<String> {
    feed.iter().filter_map(|b| render_batch(b)).collect()
}

// ── The replay differential ──────────────────────────────────────────────────

/// Apply `requests` cleanly (path a) vs. with at-least-once duplication (path
/// b: immediate echo of every request, then a stale-checkpoint replay of the
/// tail from `p`), on a fresh store of backend `B` each. Assert both reach the
/// same final quad set.
fn replay_converges<B: FullBackend + Default>(requests: &[String], p: usize) {
    // Path (a): one clean pass.
    let mut clean = B::default();
    for r in requests {
        apply(&mut clean, r);
    }

    // Path (b): every request delivered twice back-to-back (immediate echo),
    // then the tail requests[p..] redelivered as a block (stale checkpoint).
    let mut replay = B::default();
    for r in requests {
        apply(&mut replay, r);
        apply(&mut replay, r);
    }
    for r in &requests[p..] {
        apply(&mut replay, r);
    }

    assert_eq!(
        dump(&clean),
        dump(&replay),
        "at-least-once replay diverged from a clean pass"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn at_least_once_feed_replay_converges(
        (feed, p) in arb_feed().prop_flat_map(|feed| {
            let n = feed.len().max(1);
            (Just(feed), 0..n)
        })
    ) {
        let requests = render_feed(&feed);
        // `p` indexes the raw feed; clamp to the rendered request count (empty
        // batches render to nothing, so the two lengths can differ).
        let p = p.min(requests.len());
        replay_converges::<MemStore>(&requests, p);
        replay_converges::<HornBackend>(&requests, p);
    }
}

// ── One batch per operation, in request order ────────────────────────────────

fn one_batch_per_operation<B: FullBackend + Default>() {
    let q = "<http://ex/a> <http://ex/p> <http://ex/b>";

    // del ; ins ; del — the later delete wins, so `q` ends absent.
    let mut s1 = B::default();
    apply(
        &mut s1,
        &format!("DELETE DATA {{ {q} }} ; INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }}"),
    );
    assert!(
        dump(&s1).is_empty(),
        "del;ins;del must end absent (each op is its own batch, later delete wins)"
    );

    // ins ; del ; ins — the mirror ends present.
    let mut s2 = B::default();
    apply(
        &mut s2,
        &format!("INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }} ; INSERT DATA {{ {q} }}"),
    );
    assert_eq!(
        dump(&s2).len(),
        1,
        "ins;del;ins must end present (operations are not collapsed to a net delta)"
    );
}

#[test]
fn one_batch_per_operation_mem() {
    one_batch_per_operation::<MemStore>();
}
#[test]
fn one_batch_per_operation_horn() {
    one_batch_per_operation::<HornBackend>();
}

// ── Non-canonical literal identity pin ───────────────────────────────────────

/// Quad identity is lexical term equality (SPEC-28 S6): a `DELETE DATA` of
/// `"1"^^xsd:integer` must not touch the `"01"^^xsd:integer` quad. Rendered
/// through the SPARQL update path on both backends.
fn non_canonical_literal_identity<B: FullBackend + Default>() {
    let mut store = B::default();
    apply(
        &mut store,
        &format!(
            "INSERT DATA {{ GRAPH <{G1}> {{ <http://ex/pin-s> <http://ex/pin-p> \"01\"^^<{XSD_INTEGER}> }} }}"
        ),
    );
    // Deleting the canonical form must be a no-op — different lexical term.
    apply(
        &mut store,
        &format!(
            "DELETE DATA {{ GRAPH <{G1}> {{ <http://ex/pin-s> <http://ex/pin-p> \"1\"^^<{XSD_INTEGER}> }} }}"
        ),
    );

    let quads = dump(&store);
    let non_canonical = (
        Some(G1.to_owned()),
        Term::Iri("http://ex/pin-s".to_owned()),
        Term::Iri("http://ex/pin-p".to_owned()),
        Term::Literal(format!("\"01\"^^<{XSD_INTEGER}>")),
    );
    let canonical = (
        Some(G1.to_owned()),
        Term::Iri("http://ex/pin-s".to_owned()),
        Term::Iri("http://ex/pin-p".to_owned()),
        Term::Literal(format!("\"1\"^^<{XSD_INTEGER}>")),
    );
    assert!(
        quads.contains(&non_canonical),
        "the non-canonical \"01\" quad must survive a delete of the canonical \"1\" form"
    );
    assert!(
        !quads.contains(&canonical),
        "the canonical \"1\" form was never inserted and must not appear"
    );
}

#[test]
fn non_canonical_literal_identity_mem() {
    non_canonical_literal_identity::<MemStore>();
}
#[test]
fn non_canonical_literal_identity_horn() {
    non_canonical_literal_identity::<HornBackend>();
}
