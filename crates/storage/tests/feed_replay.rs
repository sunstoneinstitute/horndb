//! SPEC-28 S6/S7 storage-level replay differential (acceptance 7): a
//! proptest that generates a quad-grain feed — batches of adds/dels over a
//! small term space, so the same quad collides across batches — and applies
//! it two ways: (a) once, cleanly, in order; (b) with a random
//! duplicated-batch replay from a stale point mid-stream, simulating an
//! at-least-once feed that re-delivers already-applied batches after a
//! restart. Asserts:
//!   - quad-set equality of the (a) and (b) final states;
//!   - the replayed prefix reports exactly zero counts, EXCEPT for the
//!     documented case where a batch's own del+add re-touches an
//!     already-visible quad (Task 1's `dels_before_adds_within_batch`
//!     contract) — checked via an independent oracle rather than a blind
//!     zero, since a literal always-zero assertion is not actually what the
//!     store promises (see `Oracle`'s doc);
//!   - the dictionary's non-canonical-literal identity survives arbitrary
//!     churn: `"01"^^xsd:integer` and `"1"^^xsd:integer` stay distinct
//!     quads.
//!
//! This is storage-level, exercising `Store::apply_quads` directly. The
//! SPARQL-level twin (the same feed shape rendered as `DELETE
//! DATA;INSERT DATA` requests, which additionally pins one-batch-per-request
//! ordering) is `crates/sparql/tests/update_feed_replay.rs`.

use horndb_storage::{ApplyReport, GraphId, Store, DEFAULT_GRAPH};
use oxrdf::{Literal, NamedNode, NamedNodeRef, Term};
use proptest::prelude::*;

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn typed_int(lexical: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(
        lexical,
        NamedNodeRef::new(XSD_INTEGER).unwrap(),
    ))
}

// --- a deliberately small term space, so batches collide (quads repeat,
// del-then-add of the same quad happens, etc.) ---

const N_SUBJECTS: u8 = 3;
const N_PREDICATES: u8 = 2;
const N_OBJECTS: u8 = 3;
const N_GRAPHS: u8 = 2; // slot 0 = default graph, slot 1 = named graph "g1"

fn subj(i: u8) -> Term {
    iri(&format!("http://ex/s{i}"))
}

fn pred(i: u8) -> Term {
    iri(&format!("http://ex/p{i}"))
}

fn obj(i: u8) -> Term {
    iri(&format!("http://ex/o{i}"))
}

fn named_graph_iri() -> Term {
    iri("http://ex/g1")
}

/// Resolve a graph slot to a `GraphId` on `store`, interning the named-graph
/// IRI as needed. `Store::apply_quads` requires a pre-interned `GraphId` on
/// both the del and the add side, so every quad — not just adds — is
/// resolved through this.
fn graph_id(store: &Store, slot: u8) -> GraphId {
    if slot == 0 {
        DEFAULT_GRAPH
    } else {
        store.intern_graph_uri(&named_graph_iri()).unwrap()
    }
}

// --- feed shape: batches of (dels, adds) over quad-index tuples ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct QuadIdx {
    g: u8,
    s: u8,
    p: u8,
    o: u8,
}

#[derive(Debug, Clone)]
struct Batch {
    dels: Vec<QuadIdx>,
    adds: Vec<QuadIdx>,
}

fn quad_idx_strategy() -> impl Strategy<Value = QuadIdx> {
    (0..N_GRAPHS, 0..N_SUBJECTS, 0..N_PREDICATES, 0..N_OBJECTS).prop_map(|(g, s, p, o)| QuadIdx {
        g,
        s,
        p,
        o,
    })
}

/// A batch's `adds` sometimes echoes one of its own `dels` — a deliberate
/// del+add of the same quad within one batch, rather than relying on luck
/// from the small term space alone (the design calls this shape out
/// explicitly).
fn batch_strategy() -> impl Strategy<Value = Batch> {
    (
        proptest::collection::vec(quad_idx_strategy(), 0..4),
        proptest::collection::vec(quad_idx_strategy(), 0..4),
        proptest::collection::vec(any::<bool>(), 0..4),
    )
        .prop_map(|(dels, extra_adds, echoes)| {
            let mut adds = extra_adds;
            for (d, echo) in dels.iter().zip(echoes.iter()) {
                if *echo {
                    adds.push(*d);
                }
            }
            Batch { dels, adds }
        })
}

fn feed_strategy() -> impl Strategy<Value = Vec<Batch>> {
    proptest::collection::vec(batch_strategy(), 1..12)
}

fn resolve(store: &Store, q: &QuadIdx) -> (GraphId, Term, Term, Term) {
    (graph_id(store, q.g), subj(q.s), pred(q.p), obj(q.o))
}

fn apply_batch(store: &Store, batch: &Batch) -> ApplyReport {
    let dels: Vec<_> = batch.dels.iter().map(|q| resolve(store, q)).collect();
    let adds: Vec<_> = batch.adds.iter().map(|q| resolve(store, q)).collect();
    store.apply_quads(&dels, &adds).unwrap()
}

/// An independent, `HashSet`-based reference model of `ApplyReport`
/// counting, built straight from the field docs on
/// [`horndb_storage::tier::ApplyReport`] and [`horndb_storage::Tier`]:
/// `retracted` counts del targets actually visible beforehand; `inserted`
/// counts add targets not visible once the dels (same batch) have applied;
/// duplicate targets within one side of one batch count once.
///
/// A literal "replaying a batch always reports (0, 0)" is NOT always true —
/// `memory_tier.rs`'s own `dels_before_adds_within_batch` test pins that a
/// batch containing an internal del+add of an already-visible quad reports
/// `(1, 1)` on every application, replay included, because the del
/// genuinely un-lives the row before the add re-lives it. `Oracle` predicts
/// the exact expected report from the same rules, so comparing the real
/// store's report against it is correct in that case too, while reducing to
/// the ordinary zero-count check whenever a replayed batch has no such
/// internal or cross-batch quad reuse (the common case this test exercises).
#[derive(Default, Clone)]
struct Oracle {
    visible: std::collections::HashSet<QuadIdx>,
}

impl Oracle {
    fn apply(&mut self, batch: &Batch) -> ApplyReport {
        let del_targets: std::collections::HashSet<QuadIdx> = batch.dels.iter().copied().collect();
        let mut retracted = 0usize;
        for d in &del_targets {
            if self.visible.remove(d) {
                retracted += 1;
            }
        }
        let add_targets: std::collections::HashSet<QuadIdx> = batch.adds.iter().copied().collect();
        let mut inserted = 0usize;
        for a in &add_targets {
            if self.visible.insert(*a) {
                inserted += 1;
            }
        }
        ApplyReport {
            retracted,
            inserted,
        }
    }
}

/// Applies `batches` to `store` in order, keeping `oracle` in lockstep, and
/// returns each batch's (real, oracle-expected) `ApplyReport` pair for the
/// caller to assert on.
fn apply_and_track(
    store: &Store,
    oracle: &mut Oracle,
    batches: &[Batch],
) -> Vec<(ApplyReport, ApplyReport)> {
    batches
        .iter()
        .map(|batch| {
            let expected = oracle.apply(batch);
            let real = apply_batch(store, batch);
            (real, expected)
        })
        .collect()
}

/// Every visible quad in `g`, Debug-canonicalized and sorted, for a
/// store-independent equality check. Debug formatting keeps
/// `"01"^^xsd:integer` and `"1"^^xsd:integer` distinct (lexical form is part
/// of `Literal`'s Debug output), matching the dictionary's own identity
/// contract.
fn canonical_graph_rows(store: &Store, g: GraphId) -> Vec<String> {
    let mut rows: Vec<String> = store
        .snapshot()
        .scan_graph(g)
        .unwrap()
        .iter()
        .map(|t| format!("{t:?}"))
        .collect();
    rows.sort();
    rows
}

/// The whole store's visible state as (default-graph rows, "g1" rows) — the
/// two graph slots this test's term space ever touches.
fn canonical_state(store: &Store) -> (Vec<String>, Vec<String>) {
    let default_rows = canonical_graph_rows(store, DEFAULT_GRAPH);
    let g1 = graph_id(store, 1);
    let g1_rows = canonical_graph_rows(store, g1);
    (default_rows, g1_rows)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `k` is how far the feed had been processed (in order) before a
    /// restart; `stale` (clamped to `<= k`) is the last durably-recorded
    /// checkpoint. On restart the feed is re-delivered starting at `stale`,
    /// so `feed[stale..k]` is applied a second time before the feed
    /// continues normally from `k` — an at-least-once feed's duplicated
    /// prefix.
    #[test]
    fn at_least_once_feed_replay_converges(
        feed in feed_strategy(),
        k_raw in 0usize..=16,
        stale_raw in 0usize..=16,
    ) {
        let n = feed.len();
        let k = k_raw.min(n);
        let stale = stale_raw.min(k);

        // (a) apply the feed once, cleanly, in order. Cross-check every
        // batch's report against the independent oracle.
        let store_a = Store::in_memory();
        let mut oracle_a = Oracle::default();
        for (real, expected) in apply_and_track(&store_a, &mut oracle_a, &feed) {
            prop_assert_eq!(
                real, expected,
                "ApplyReport must match the store's own dels-before-adds counting contract"
            );
        }

        // (b) apply feed[..k], then re-deliver feed[stale..k] (the
        // duplicated, already-converged prefix — an at-least-once feed's
        // replayed batches), then continue with feed[k..]. The replayed
        // batches must report exactly what the oracle predicts: zero counts
        // for an ordinary already-converged batch, or the documented
        // toggle-driven non-zero count for a batch that legitimately
        // deletes-then-re-adds an already-visible quad (see `Oracle`'s doc).
        let store_b = Store::in_memory();
        let mut oracle_b = Oracle::default();
        for (real, expected) in apply_and_track(&store_b, &mut oracle_b, &feed[..k]) {
            prop_assert_eq!(real, expected);
        }
        for (real, expected) in apply_and_track(&store_b, &mut oracle_b, &feed[stale..k]) {
            prop_assert_eq!(
                real, expected,
                "re-applying an already-applied batch must match the store's own counting \
                 contract exactly (zero, unless a genuine toggle re-touches a row)"
            );
        }
        for (real, expected) in apply_and_track(&store_b, &mut oracle_b, &feed[k..]) {
            prop_assert_eq!(real, expected);
        }

        // Quad-set equality: the clean run and the at-least-once-replayed
        // run must converge to the same final state.
        prop_assert_eq!(
            canonical_state(&store_a),
            canonical_state(&store_b),
            "clean and replayed feeds must converge to the same quad set"
        );

        // Non-canonical-literal identity pin (SPEC-28 S6): whatever state
        // the fuzzed feed left store_a in, inserting "01"^^xsd:integer and
        // then deleting "1"^^xsd:integer (same s, p) must not touch it — the
        // dictionary's identity-preserving inline-int path keeps them
        // distinct quads. A subject/predicate pair outside the fuzzed term
        // space isolates this check from random churn on the same cell.
        let pin_s = iri("http://ex/pin-s");
        let pin_p = iri("http://ex/pin-p");
        let non_canonical = typed_int("01");
        let canonical = typed_int("1");
        store_a
            .apply_quads(
                &[],
                &[(DEFAULT_GRAPH, pin_s.clone(), pin_p.clone(), non_canonical.clone())],
            )
            .unwrap();
        let pin_report = store_a
            .apply_quads(&[(DEFAULT_GRAPH, pin_s.clone(), pin_p.clone(), canonical)], &[])
            .unwrap();
        prop_assert_eq!(
            pin_report.retracted, 0,
            "\"1\"^^xsd:integer must not retract a stored \"01\"^^xsd:integer quad"
        );
        let pin_rows = store_a.scan_predicate(DEFAULT_GRAPH, &pin_p).unwrap();
        prop_assert!(
            pin_rows.iter().any(|(s, o)| *s == pin_s && *o == non_canonical),
            "the non-canonical \"01\"^^xsd:integer quad must still be present"
        );
    }
}
