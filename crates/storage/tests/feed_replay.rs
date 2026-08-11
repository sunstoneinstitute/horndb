//! SPEC-28 S6/S7 acceptance criterion 7 (see
//! `docs/plans/PLAN-28-04-named-graph-update.md`, "The replay
//! differential"): an at-least-once change feed — one that can redeliver or
//! duplicate batches, e.g. after a consumer restarts from a checkpoint that
//! lags its real progress — must converge to the same store state as one
//! clean, no-duplicates application. `Tier::apply_quad_batch`
//! (`crates/storage/src/tier.rs`) makes this true by construction: applying
//! a batch is the set transformation `T(S) = (S \ dels) ∪ adds`, which is
//! idempotent (`T(T(S)) == T(S)`), and composing a contiguous run of such
//! transformations is idempotent too. This test is a property-based
//! differential check of that guarantee against Task 1's implementation
//! (commit c6a2494) — any divergence found here is a Task-1 bug, not a
//! flaw in the test's assertions.
//!
//! Two duplication mechanisms are exercised, chosen because each is
//! provably a no-op given the algebra above (so a failure here is real, not
//! an artifact of an over-strong assertion):
//!
//! 1. **Immediate echo** — every batch is applied, then immediately
//!    re-applied before the next batch runs (the textbook "redelivered
//!    before ack" case). Nothing intervenes between the two deliveries, so
//!    each echo must report zero counts and must not bump the commit
//!    version.
//! 2. **Stale-checkpoint tail replay** — after the whole feed has been
//!    applied once, the tail from a random "stale" checkpoint `p` through
//!    the end of the feed is redelivered as a block, modelling a restart
//!    whose checkpoint lagged the consumer's actual progress. Only the
//!    *net* effect of the whole tail replay is guaranteed to be a no-op
//!    (an interior batch of the tail can show a nonzero count if its
//!    target quads collide with a later batch in the same tail — that
//!    collision is exactly what the small term space is for), so this
//!    mechanism is checked only for final-state convergence, not
//!    per-batch zero counts.

use horndb_storage::{ApplyReport, GraphId, Store, DEFAULT_GRAPH};
use oxrdf::{Literal, NamedNode, Term};
use proptest::prelude::*;
use std::collections::HashSet;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn xsd_integer_literal(lexical: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(
        lexical,
        NamedNode::new("http://www.w3.org/2001/XMLSchema#integer").unwrap(),
    ))
}

// --- a small, deliberately collision-prone term space ------------------
//
// 2 graphs x 3 subjects x 2 predicates x 3 objects = 36 possible quads, fed
// into batches of 1-3 actions over a 2-8 batch feed (up to 24 draws): the
// birthday-paradox math on 24 draws from 36 slots makes cross-batch
// collisions (the same quad targeted by two different batches) common, not
// merely possible.

const N_SUBJECTS: usize = 3;
const N_PREDICATES: usize = 2;
const N_OBJECTS: usize = 3;

fn subjects() -> Vec<Term> {
    (0..N_SUBJECTS)
        .map(|i| iri(&format!("http://ex/s{i}")))
        .collect()
}

fn predicates() -> Vec<Term> {
    (0..N_PREDICATES)
        .map(|i| iri(&format!("http://ex/p{i}")))
        .collect()
}

fn objects() -> Vec<Term> {
    (0..N_OBJECTS)
        .map(|i| iri(&format!("http://ex/o{i}")))
        .collect()
}

/// Which of the term space's two graphs a quad targets. Resolved to a
/// concrete `GraphId` per store at apply time — each `Store` interns
/// `http://ex/g1` independently, so a raw `GraphId` is not portable across
/// the two stores this test builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphSel {
    Default,
    Named,
}

type QuadIdx = (GraphSel, usize, usize, usize); // (graph, subject, predicate, object)

fn arb_quad_idx() -> impl Strategy<Value = QuadIdx> {
    (
        prop_oneof![Just(GraphSel::Default), Just(GraphSel::Named)],
        0..N_SUBJECTS,
        0..N_PREDICATES,
        0..N_OBJECTS,
    )
}

#[derive(Clone, Debug)]
enum Action {
    Add(QuadIdx),
    Del(QuadIdx),
    /// Del-then-add of the SAME quad within one batch — acceptance-7's
    /// explicit "at least the possibility of a del+add of the same quad in
    /// one batch" requirement.
    Both(QuadIdx),
}

fn arb_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        arb_quad_idx().prop_map(Action::Add),
        arb_quad_idx().prop_map(Action::Del),
        arb_quad_idx().prop_map(Action::Both),
    ]
}

fn arb_batch() -> impl Strategy<Value = Vec<Action>> {
    proptest::collection::vec(arb_action(), 1..=3)
}

fn arb_feed() -> impl Strategy<Value = Vec<Vec<Action>>> {
    proptest::collection::vec(arb_batch(), 2..=8)
}

/// A resolved quad, graph still symbolic (see `GraphSel`).
type Quad = (GraphSel, Term, Term, Term);

fn resolve_quad(idx: QuadIdx, subj: &[Term], pred: &[Term], obj: &[Term]) -> Quad {
    let (g, s, p, o) = idx;
    (g, subj[s].clone(), pred[p].clone(), obj[o].clone())
}

fn resolve_batch(
    actions: &[Action],
    subj: &[Term],
    pred: &[Term],
    obj: &[Term],
) -> (Vec<Quad>, Vec<Quad>) {
    let mut dels = Vec::new();
    let mut adds = Vec::new();
    for act in actions {
        match act {
            Action::Add(q) => adds.push(resolve_quad(*q, subj, pred, obj)),
            Action::Del(q) => dels.push(resolve_quad(*q, subj, pred, obj)),
            Action::Both(q) => {
                let quad = resolve_quad(*q, subj, pred, obj);
                dels.push(quad.clone());
                adds.push(quad);
            }
        }
    }
    (dels, adds)
}

/// Bind symbolic `Quad`s to one store's concrete `GraphId`s.
fn bind(quads: &[Quad], g1: GraphId) -> Vec<(GraphId, Term, Term, Term)> {
    quads
        .iter()
        .map(|(g, s, p, o)| {
            let gid = match g {
                GraphSel::Default => DEFAULT_GRAPH,
                GraphSel::Named => g1,
            };
            (gid, s.clone(), p.clone(), o.clone())
        })
        .collect()
}

/// Every visible quad across both term-space graphs, decoded to `Term`s (so
/// two independently-dictionaried stores are directly comparable) and
/// tagged with which graph it came from.
fn visible_quads(store: &Store, g1: GraphId) -> HashSet<(bool, Term, Term, Term)> {
    let snap = store.snapshot();
    let mut out = HashSet::new();
    for (s, p, o) in snap.scan_graph(DEFAULT_GRAPH).unwrap() {
        out.insert((false, s, p, o));
    }
    for (s, p, o) in snap.scan_graph(g1).unwrap() {
        out.insert((true, s, p, o));
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Acceptance-7: at-least-once replay (echoed and stale-checkpoint
    /// duplicate deliveries) converges to the same visible store state as
    /// one clean application; the identity pin
    /// (`"01"^^xsd:integer` != `"1"^^xsd:integer`) survives the whole path.
    #[test]
    fn at_least_once_feed_replay_converges(
        (random_feed, p) in arb_feed().prop_flat_map(|feed| {
            let len = feed.len();
            (Just(feed), 0..len)
        })
    ) {
        let subj = subjects();
        let pred = predicates();
        let obj = objects();

        // Deterministic identity-pin tail, appended to every generated feed
        // so the pin is exercised on every case rather than only when the
        // random generator happens to draw the literal pair (SPEC-28 S6:
        // quad identity is lexical term equality, no value normalization).
        let pin_s = iri("http://ex/pin-s");
        let pin_p = iri("http://ex/pin-p");
        let pin_non_canonical = xsd_integer_literal("01");
        let pin_canonical = xsd_integer_literal("1");
        let pin_add: (Vec<Quad>, Vec<Quad>) = (
            vec![],
            vec![(
                GraphSel::Named,
                pin_s.clone(),
                pin_p.clone(),
                pin_non_canonical.clone(),
            )],
        );
        let pin_del: (Vec<Quad>, Vec<Quad>) = (
            vec![(
                GraphSel::Named,
                pin_s.clone(),
                pin_p.clone(),
                pin_canonical.clone(),
            )],
            vec![],
        );

        let mut full_feed: Vec<(Vec<Quad>, Vec<Quad>)> = random_feed
            .iter()
            .map(|batch| resolve_batch(batch, &subj, &pred, &obj))
            .collect();
        full_feed.push(pin_add);
        full_feed.push(pin_del);
        let pin_del_index = full_feed.len() - 1;

        // --- path (a): one clean pass, no duplicates ---
        let store_a = Store::in_memory();
        let g1_a = store_a.intern_graph_uri(&iri("http://ex/g1")).unwrap();
        let mut pin_del_report_a = None;
        for (idx, (dels, adds)) in full_feed.iter().enumerate() {
            let report = store_a
                .apply_quads(&bind(dels, g1_a), &bind(adds, g1_a))
                .unwrap();
            if idx == pin_del_index {
                pin_del_report_a = Some(report);
            }
        }
        // The identity pin, checked directly against the real ApplyReport:
        // deleting the canonical literal form must be a genuine 0-count
        // no-op, never touching the non-canonical quad actually inserted.
        prop_assert_eq!(
            pin_del_report_a,
            Some(ApplyReport { retracted: 0, inserted: 0 }),
            "deleting the canonical literal must not retract the non-canonical quad"
        );

        // --- path (b): at-least-once replay ---
        let store_b = Store::in_memory();
        let g1_b = store_b.intern_graph_uri(&iri("http://ex/g1")).unwrap();

        // Mechanism 1: immediate echo of every batch. State-level, an echo
        // is ALWAYS a no-op (nothing intervenes between the two
        // deliveries): `T(T(S)) == T(S)` for `T(S) = (S \ dels) ∪ adds`,
        // regardless of whether `dels` and `adds` overlap. But the
        // REPORTED COUNTS are zero only when they don't overlap: per the
        // documented contract (`Tier::apply_quad_batch`, pinned by Task 1's
        // own `dels_before_adds_within_batch` test), a quad present in both
        // `dels` and `adds` of the SAME batch always counts once retracted
        // (if visible before) and once inserted (unconditionally, since the
        // del makes it invisible immediately before the add) — every time
        // that batch is applied, replay included. So the zero-count
        // assertion is scoped to batches with no del/add overlap; the echo
        // itself still runs unconditionally for state-level coverage.
        for (dels, adds) in &full_feed {
            let dels_b = bind(dels, g1_b);
            let adds_b = bind(adds, g1_b);
            store_b.apply_quads(&dels_b, &adds_b).unwrap();
            let overlaps = dels_b.iter().any(|d| adds_b.contains(d));
            if overlaps {
                store_b.apply_quads(&dels_b, &adds_b).unwrap();
                continue;
            }
            let version_before_echo = store_b.snapshot().version();
            let echo = store_b.apply_quads(&dels_b, &adds_b).unwrap();
            prop_assert_eq!(
                echo,
                ApplyReport { retracted: 0, inserted: 0 },
                "an immediate duplicate delivery of a non-overlapping batch must be a zero-count no-op"
            );
            prop_assert_eq!(
                store_b.snapshot().version(),
                version_before_echo,
                "a zero-count no-op batch must not bump the commit version"
            );
        }

        // Mechanism 2: stale-checkpoint tail replay — redeliver
        // full_feed[p..] as a block after the whole feed has already been
        // applied once above. Models a restart whose checkpoint `p` lagged
        // the consumer's real progress (the end of the feed). Individual
        // counts in this replay are not asserted (an interior batch can
        // show a real, nonzero count when its quads collide with a later
        // batch within the same tail) — only the final state matters here.
        for (dels, adds) in &full_feed[p..] {
            store_b
                .apply_quads(&bind(dels, g1_b), &bind(adds, g1_b))
                .unwrap();
        }

        // --- convergence: quad-set equality between the two paths ---
        let final_a = visible_quads(&store_a, g1_a);
        let final_b = visible_quads(&store_b, g1_b);

        // The identity pin's non-canonical quad must survive the whole
        // replay path; the canonical form was never inserted and must not
        // appear (it must not have been conflated with the non-canonical
        // one anywhere along the way). Checked before the set-equality
        // assert below consumes `final_b`.
        prop_assert!(
            final_b.contains(&(true, pin_s.clone(), pin_p.clone(), pin_non_canonical)),
            "the non-canonical literal quad must survive the whole replay path"
        );
        prop_assert!(
            !final_b.contains(&(true, pin_s, pin_p, pin_canonical)),
            "the canonical literal form was never inserted and must not appear"
        );

        prop_assert_eq!(
            final_a, final_b,
            "at-least-once replay must converge to the same visible state as one clean pass"
        );
    }
}
