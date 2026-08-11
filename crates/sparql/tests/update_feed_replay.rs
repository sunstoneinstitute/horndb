//! SPEC-28 S6/S7 SPARQL-level replay differential (acceptance 7): the
//! `crates/storage/tests/feed_replay.rs` twin, one layer up. A proptest
//! renders the same quad-grain feed shape — batches of adds/dels over a
//! small, deliberately-colliding term space, including a batch whose adds
//! echo one of its own dels — as SPARQL Update requests (`DELETE DATA` /
//! `INSERT DATA`, one operation per non-empty side of a batch, mirroring
//! SPEC-28 S4's "one operation = one store commit") instead of calling
//! `Store::apply_quads` directly. The feed is applied (a) once, cleanly, in
//! order, and (b) with a duplicated-batch replay from a stale point — an
//! at-least-once feed re-delivering already-applied batches after a
//! restart. Asserts the two runs converge to the same full quad set
//! (default graph + every named graph), on both backends.
//!
//! Also pins the ordering property that only exists at this layer: because
//! each operation in a multi-op request is its own commit (never a fused
//! dels-before-adds batch across the whole request), a request `DELETE
//! DATA{q} ; INSERT DATA{q} ; DELETE DATA{q}` must end with `q` ABSENT — a
//! wrong implementation that collapsed the request into one
//! dels-before-adds batch would instead leave `q` PRESENT. Reuses the
//! apply/load/scan idiom from `update_named_graph.rs`.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, GraphNamedNode, Store, StoreGraphTarget};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use proptest::prelude::*;
use std::collections::BTreeSet;

// --- a deliberately small term space, so batches collide (quads repeat,
// del-then-add of the same quad happens, etc.) -- mirrors
// `crates/storage/tests/feed_replay.rs`.

const N_SUBJECTS: u8 = 3;
const N_PREDICATES: u8 = 2;
const N_OBJECTS: u8 = 3;
const N_GRAPHS: u8 = 2; // slot 0 = default graph, slot 1 = named graph "g1"
const NAMED_GRAPH: &str = "http://ex/g1";

fn subj(i: u8) -> String {
    format!("http://ex/s{i}")
}
fn pred(i: u8) -> String {
    format!("http://ex/p{i}")
}
fn obj(i: u8) -> String {
    format!("http://ex/o{i}")
}

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

/// A batch's `adds` sometimes echoes one of its own `dels` -- a deliberate
/// del+add of the same quad within one batch (see module docs).
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

/// Render a batch's ground quads as one SPARQL quad-data block's body,
/// routing graph-slot-1 quads through a `GRAPH <..> { }` sub-block (legal in
/// `INSERT DATA`/`DELETE DATA`'s `QuadData` grammar, exercised already in
/// `update_named_graph.rs::insert_delete_data_graph_blocks`).
fn render_quads(idxs: &[QuadIdx]) -> String {
    let mut default_lines = String::new();
    let mut named_lines = String::new();
    for q in idxs {
        let line = format!("<{}> <{}> <{}> .\n", subj(q.s), pred(q.p), obj(q.o));
        if q.g == 0 {
            default_lines.push_str(&line);
        } else {
            named_lines.push_str(&line);
        }
    }
    let mut out = default_lines;
    if !named_lines.is_empty() {
        out.push_str(&format!("GRAPH <{NAMED_GRAPH}> {{\n{named_lines}}}\n"));
    }
    out
}

/// Render a batch as a SPARQL Update request: one `DELETE DATA {}` op (if
/// `dels` is non-empty), then one `INSERT DATA {}` op (if `adds` is
/// non-empty), semicolon-joined -- each its own top-level operation / store
/// commit (SPEC-28 S4), never a fused dels-before-adds batch. `None` when
/// the batch carries no quads at all (nothing to apply).
fn render_request(batch: &Batch) -> Option<String> {
    let mut ops = Vec::new();
    if !batch.dels.is_empty() {
        ops.push(format!("DELETE DATA {{\n{}}}", render_quads(&batch.dels)));
    }
    if !batch.adds.is_empty() {
        ops.push(format!("INSERT DATA {{\n{}}}", render_quads(&batch.adds)));
    }
    if ops.is_empty() {
        None
    } else {
        Some(ops.join(" ;\n"))
    }
}

fn apply_batch<B: FullBackend>(store: &mut B, batch: &Batch) {
    let Some(req) = render_request(batch) else {
        return;
    };
    let parsed = parse_update(&req).unwrap_or_else(|e| panic!("parse {req}: {e}"));
    apply_update(&parsed, store).unwrap_or_else(|e| panic!("apply {req}: {e}"));
}

fn apply_feed<B: FullBackend>(store: &mut B, feed: &[Batch]) {
    for batch in feed {
        apply_batch(store, batch);
    }
}

fn term_str(t: &Term) -> String {
    match t {
        Term::Iri(s) => s.clone(),
        Term::Literal(s) => s.clone(),
        Term::BlankNode(s) => format!("_:{s}"),
        other => panic!("unexpected term in generated feed data: {other:?}"),
    }
}

/// Every quad in `store` (default graph + every named graph), as a
/// store-independent comparable set.
fn dump<B: FullBackend>(store: &B) -> BTreeSet<(Option<String>, String, String, String)> {
    let mut out = BTreeSet::new();
    for (s, p, o) in store
        .scan_graph_quads(&StoreGraphTarget::DefaultGraph)
        .unwrap()
    {
        out.insert((None, term_str(&s), term_str(&p), term_str(&o)));
    }
    for g in Store::named_graphs(store) {
        let tgt = StoreGraphTarget::NamedNode(GraphNamedNode::new_unchecked(&g));
        for (s, p, o) in store.scan_graph_quads(&tgt).unwrap() {
            out.insert((Some(g.clone()), term_str(&s), term_str(&p), term_str(&o)));
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `k` is how far the feed had been processed (in order) before a
    /// restart; `stale` (clamped to `<= k`) is the last durably-recorded
    /// checkpoint. On restart the feed is re-delivered starting at `stale`,
    /// so `feed[stale..k]` is applied a second time before the feed
    /// continues normally from `k` -- an at-least-once feed's duplicated
    /// prefix (mirrors `crates/storage/tests/feed_replay.rs`).
    #[test]
    fn at_least_once_feed_replay_converges(
        feed in feed_strategy(),
        k_raw in 0usize..=16,
        stale_raw in 0usize..=16,
    ) {
        let n = feed.len();
        let k = k_raw.min(n);
        let stale = stale_raw.min(k);

        // (a) apply the feed once, cleanly, in order.
        let mut mem_a = MemStore::default();
        apply_feed(&mut mem_a, &feed);
        let mut horn_a = HornBackend::default();
        apply_feed(&mut horn_a, &feed);

        // (b) feed[..k], then re-deliver feed[stale..k] (the duplicated,
        // already-converged prefix), then continue with feed[k..].
        let mut mem_b = MemStore::default();
        apply_feed(&mut mem_b, &feed[..k]);
        apply_feed(&mut mem_b, &feed[stale..k]);
        apply_feed(&mut mem_b, &feed[k..]);
        let mut horn_b = HornBackend::default();
        apply_feed(&mut horn_b, &feed[..k]);
        apply_feed(&mut horn_b, &feed[stale..k]);
        apply_feed(&mut horn_b, &feed[k..]);

        // Quad-set equality: the clean run and the at-least-once-replayed
        // run must converge to the same final state, on both backends.
        prop_assert_eq!(
            dump(&mem_a), dump(&mem_b),
            "MemStore: clean and replayed feeds must converge to the same quad set"
        );
        prop_assert_eq!(
            dump(&horn_a), dump(&horn_b),
            "HornBackend: clean and replayed feeds must converge to the same quad set"
        );
        // Bonus differential: the two backends must agree with each other
        // on the clean run too.
        prop_assert_eq!(
            dump(&mem_a), dump(&horn_a),
            "MemStore and HornBackend must agree on the clean feed's final state"
        );
    }
}

// ── One-operation-per-commit ordering pin (SPEC-28 S4) ───────────────────

fn one_op_per_commit_last_write_wins<B: FullBackend + Default>() {
    // A single multi-op request DELETE{q};INSERT{q};DELETE{q} must end with
    // q ABSENT: each operation is its own store commit (S4), so the net
    // effect is del, then add, then del again -- last write wins. A wrong
    // implementation that fused the whole request into one
    // dels-before-adds batch would instead compute dels={q,q}, adds={q} and
    // leave q PRESENT.
    let mut store = B::default();
    let req = "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
               INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
               DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }";
    let parsed = parse_update(req).unwrap();
    apply_update(&parsed, &mut store).unwrap();
    assert!(
        dump(&store).is_empty(),
        "q must be absent after DELETE;INSERT;DELETE"
    );
}

#[test]
fn one_op_per_commit_last_write_wins_mem() {
    one_op_per_commit_last_write_wins::<MemStore>();
}
#[test]
fn one_op_per_commit_last_write_wins_horn() {
    one_op_per_commit_last_write_wins::<HornBackend>();
}
