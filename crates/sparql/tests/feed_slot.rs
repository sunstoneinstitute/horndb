//! SPEC-30 P1 — the applied-position slot (§S2/S5) at the SPARQL Update
//! layer: `PLAN-30-01-applied-position-slot.md` Task 1 + Task 4.
//!
//! Drives `update::apply_update_with_feed` directly (the seam `/update`'s
//! HTTP handler calls into — see `tests/server_http.rs` for the HTTP-level
//! tests) against both Stage-1 backends.

use horndb_sparql::algebra::{Term, TriplePattern};
use horndb_sparql::error::SparqlError;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{
    AlgebraQuad, AlgebraTriple, ApplyCounts, Bindings, Executor, FullBackend, Pinnable, ScanScope,
    Store,
};
use horndb_sparql::feed::FeedPosition;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update_with_feed;
use horndb_sparql::{Result, SparqlConfig};
use spargebra::algebra::GraphTarget;
use spargebra::term::NamedNode;

const FEED_GRAPH: &str = "https://horndb.io/graph/feed";
const PRED_ID: &str = "https://horndb.io/ns/feed#id";
const PRED_POSITION: &str = "https://horndb.io/ns/feed#position";

fn apply_feed<B: FullBackend>(
    store: &mut B,
    request: &str,
    feed: Option<&FeedPosition>,
) -> Result<()> {
    let parsed = parse_update(request).unwrap_or_else(|e| panic!("parse `{request}`: {e}"));
    apply_update_with_feed(&parsed, store, &SparqlConfig::default(), feed)
}

/// The feed graph's quads, decoded — empty when no slot exists.
fn feed_quads<B: Store>(store: &B) -> Vec<AlgebraTriple> {
    let target = GraphTarget::NamedNode(NamedNode::new_unchecked(FEED_GRAPH));
    store.scan_graph_quads(&target).unwrap()
}

fn literal_value(o: &Term) -> Option<String> {
    match o {
        Term::Literal(lex) => {
            let body = lex.strip_prefix('"')?;
            let end = body.find('"')?;
            Some(body[..end].to_owned())
        }
        _ => None,
    }
}

fn slot_field<B: Store>(store: &B, pred: &str) -> Option<String> {
    feed_quads(store).into_iter().find_map(|(_s, p, o)| {
        matches!(&p, Term::Iri(i) if i == pred)
            .then(|| literal_value(&o))
            .flatten()
    })
}

fn slot_id<B: Store>(store: &B) -> Option<String> {
    slot_field(store, PRED_ID)
}
fn slot_position<B: Store>(store: &B) -> Option<String> {
    slot_field(store, PRED_POSITION)
}

fn data_present<B: FullBackend>(store: &B, s: &str, p: &str, o: &str) -> bool {
    store
        .scan_graph_quads(&GraphTarget::DefaultGraph)
        .unwrap()
        .contains(&(
            Term::Iri(s.to_owned()),
            Term::Iri(p.to_owned()),
            Term::Iri(o.to_owned()),
        ))
}

// ── Task 1 tests ──────────────────────────────────────────────────────────

fn slot_written_with_final_batch<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp = FeedPosition {
        id: "feed-1".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        Some(&fp),
    )
    .unwrap();

    assert_eq!(slot_id(&store).as_deref(), Some("feed-1"));
    assert_eq!(slot_position(&store).as_deref(), Some("tok-1"));
    assert!(data_present(
        &store,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b"
    ));
    assert!(data_present(
        &store,
        "http://ex/c",
        "http://ex/p",
        "http://ex/d"
    ));
}
#[test]
fn slot_written_with_final_batch_mem() {
    slot_written_with_final_batch::<MemStore>();
}
#[test]
fn slot_written_with_final_batch_horn() {
    slot_written_with_final_batch::<HornBackend>();
}

fn slot_advance_replaces_prior<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp1 = FeedPosition {
        id: "feed-1".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        Some(&fp1),
    )
    .unwrap();
    let fp2 = FeedPosition {
        id: "feed-1".into(),
        position: "tok-2".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        Some(&fp2),
    )
    .unwrap();

    // Exactly one slot subject's worth of quads (4 predicates), new token.
    assert_eq!(
        feed_quads(&store).len(),
        4,
        "old slot quads must be retracted"
    );
    assert_eq!(slot_position(&store).as_deref(), Some("tok-2"));
}
#[test]
fn slot_advance_replaces_prior_mem() {
    slot_advance_replaces_prior::<MemStore>();
}
#[test]
fn slot_advance_replaces_prior_horn() {
    slot_advance_replaces_prior::<HornBackend>();
}

fn identical_replay_is_clean<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp = FeedPosition {
        id: "feed-1".into(),
        position: "tok-1".into(),
    };
    let req = "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }";
    apply_feed(&mut store, req, Some(&fp)).unwrap();
    apply_feed(&mut store, req, Some(&fp)).unwrap();

    assert!(data_present(
        &store,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b"
    ));
    assert_eq!(
        feed_quads(&store).len(),
        4,
        "slot must not duplicate on a clean replay"
    );
    assert_eq!(slot_position(&store).as_deref(), Some("tok-1"));
}
#[test]
fn identical_replay_is_clean_mem() {
    identical_replay_is_clean::<MemStore>();
}
#[test]
fn identical_replay_is_clean_horn() {
    identical_replay_is_clean::<HornBackend>();
}

fn mismatched_feed_id_refuses_before_mutation<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp_a = FeedPosition {
        id: "feed-a".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        Some(&fp_a),
    )
    .unwrap();

    let fp_b = FeedPosition {
        id: "feed-b".into(),
        position: "tok-2".into(),
    };
    let err = apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        Some(&fp_b),
    )
    .unwrap_err();
    match err {
        SparqlError::FeedIdMismatch { slot, request } => {
            assert_eq!(slot, "feed-a");
            assert_eq!(request, "feed-b");
        }
        other => panic!("expected FeedIdMismatch, got {other:?}"),
    }
    assert!(
        !data_present(&store, "http://ex/c", "http://ex/p", "http://ex/d"),
        "no data quad may be written on a feed-id refusal"
    );
    assert_eq!(
        slot_position(&store).as_deref(),
        Some("tok-1"),
        "slot must stay unchanged"
    );
}
#[test]
fn mismatched_feed_id_refuses_before_mutation_mem() {
    mismatched_feed_id_refuses_before_mutation::<MemStore>();
}
#[test]
fn mismatched_feed_id_refuses_before_mutation_horn() {
    mismatched_feed_id_refuses_before_mutation::<HornBackend>();
}

fn empty_slot_adopts_first_id<B: FullBackend + Default>() {
    let mut store = B::default();
    assert!(slot_id(&store).is_none());
    let fp = FeedPosition {
        id: "feed-first".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        Some(&fp),
    )
    .unwrap();
    assert_eq!(slot_id(&store).as_deref(), Some("feed-first"));
}
#[test]
fn empty_slot_adopts_first_id_mem() {
    empty_slot_adopts_first_id::<MemStore>();
}
#[test]
fn empty_slot_adopts_first_id_horn() {
    empty_slot_adopts_first_id::<HornBackend>();
}

fn zero_op_request_with_position_advances_slot<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp = FeedPosition {
        id: "feed-1".into(),
        position: "tok-heartbeat".into(),
    };
    // A well-formed update with zero operations: an empty `INSERT DATA {}`.
    apply_feed(&mut store, "INSERT DATA { }", Some(&fp)).unwrap();
    assert_eq!(slot_position(&store).as_deref(), Some("tok-heartbeat"));
}
#[test]
fn zero_op_request_with_position_advances_slot_mem() {
    zero_op_request_with_position_advances_slot::<MemStore>();
}
#[test]
fn zero_op_request_with_position_advances_slot_horn() {
    zero_op_request_with_position_advances_slot::<HornBackend>();
}

fn no_headers_means_no_slot<B: FullBackend + Default>() {
    let mut store = B::default();
    let fp = FeedPosition {
        id: "feed-1".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        Some(&fp),
    )
    .unwrap();
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        None,
    )
    .unwrap();

    assert!(data_present(
        &store,
        "http://ex/c",
        "http://ex/p",
        "http://ex/d"
    ));
    assert_eq!(
        slot_position(&store).as_deref(),
        Some("tok-1"),
        "a headerless update must not touch the slot"
    );
}
#[test]
fn no_headers_means_no_slot_mem() {
    no_headers_means_no_slot::<MemStore>();
}
#[test]
fn no_headers_means_no_slot_horn() {
    no_headers_means_no_slot::<HornBackend>();
}

// ── failing_op_leaves_slot_unadvanced: a fault-injecting Store wrapper ──────

/// Wraps any [`FullBackend`] `B`, failing the `fail_at`-th (1-indexed) call to
/// [`Store::apply_quads`] with a synthetic error — simulating a mid-request
/// crash between two operations' batches (SPEC-30 §S5's "an operation error
/// or a crash mid-request leaves the slot where it was"). Every other method
/// delegates straight through.
struct FaultingBackend<B> {
    inner: B,
    calls: usize,
    fail_at: usize,
}

impl<B> FaultingBackend<B> {
    fn new(inner: B, fail_at: usize) -> Self {
        Self {
            inner,
            calls: 0,
            fail_at,
        }
    }
}

impl<B: Executor> Executor for FaultingBackend<B> {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        self.inner.scan_bgp(patterns, scope)
    }
}

impl<B: Store> Store for FaultingBackend<B> {
    fn apply_quads(
        &mut self,
        dels: Vec<AlgebraQuad>,
        adds: Vec<AlgebraQuad>,
    ) -> Result<ApplyCounts> {
        self.calls += 1;
        if self.calls == self.fail_at {
            return Err(SparqlError::Executor("injected fault".into()));
        }
        self.inner.apply_quads(dels, adds)
    }
    fn clear_graph(&mut self, graph: &GraphTarget) -> Result<usize> {
        self.inner.clear_graph(graph)
    }
    fn graph_exists(&self, graph: &str) -> bool {
        self.inner.graph_exists(graph)
    }
    fn graphs(&self) -> Vec<String> {
        self.inner.graphs()
    }
    fn scan_graph_quads(&self, graph: &GraphTarget) -> Result<Vec<AlgebraTriple>> {
        self.inner.scan_graph_quads(graph)
    }
}

impl<B: Pinnable> Pinnable for FaultingBackend<B> {
    type View = B::View;
    fn pin_read(&self) -> Self::View {
        self.inner.pin_read()
    }
}

fn failing_op_leaves_slot_unadvanced<B: FullBackend + Default>() {
    let mut store = FaultingBackend::new(B::default(), usize::MAX);
    let fp1 = FeedPosition {
        id: "feed-1".into(),
        position: "tok-1".into(),
    };
    apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        Some(&fp1),
    )
    .unwrap();
    assert_eq!(slot_position(&store).as_deref(), Some("tok-1"));

    // Two-op request; fail on the 2nd `apply_quads` call this request makes.
    store.calls = 0;
    store.fail_at = 2;
    let fp2 = FeedPosition {
        id: "feed-1".into(),
        position: "tok-2".into(),
    };
    let err = apply_feed(
        &mut store,
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> } ; \
         INSERT DATA { <http://ex/e> <http://ex/p> <http://ex/f> }",
        Some(&fp2),
    )
    .unwrap_err();
    assert!(matches!(err, SparqlError::Executor(_)));
    assert_eq!(
        slot_position(&store).as_deref(),
        Some("tok-1"),
        "a mid-request failure must leave the slot at the last committed token"
    );
}
#[test]
fn failing_op_leaves_slot_unadvanced_mem() {
    failing_op_leaves_slot_unadvanced::<MemStore>();
}
#[test]
fn failing_op_leaves_slot_unadvanced_horn() {
    failing_op_leaves_slot_unadvanced::<HornBackend>();
}
