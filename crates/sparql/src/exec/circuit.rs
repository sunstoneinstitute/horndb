//! SPEC-24 S4 (HDB-51): the DBSP circuit behind [`HornBackend`]'s write funnel.
//!
//! Every SPARQL Update operation lowers to one `assert_triple` /
//! `retract_triple` batch and one `tick()` on the [`Circuit`]. The engine is
//! then its own change-feed subscriber: it drains the records the tick just
//! published, nets them **per triple** (never one record at a time — a tick
//! may carry both a `RuleInferred` and a `ClosureInferred` record for the same
//! triple, and a withdrawal's per-triple total may reach −2), and hands the
//! net back as rows for the reserved graph [`DERIVED_GRAPH`]. The caller
//! commits them in the **same** storage batch as the base change (ADR-0018:
//! one tick, one storage commit version), so no snapshot sees base rows
//! without their consequences. Derived rows live in their own graph, not the
//! default graph, so a user `DELETE DATA` of a derived triple and an `INSERT
//! DATA` of an already-derived one can never desynchronise the circuit from
//! storage.
//!
//! **Threading model.** Tick and drain run on the *same* thread — the one
//! holding the backend's `&mut self` (the server's `AppState` write lock).
//! That is safe only because the subscription uses [`LagPolicy::DisconnectSlow`]:
//! `tick()` never blocks on this subscriber, and a tick larger than the feed
//! capacity drops the subscriber instead, which [`Wiring::apply`] detects and
//! repairs by resubscribing and resyncing the derived graph from
//! `Circuit::derived_base`. Under `LagPolicy::Block` this same-thread shape
//! would deadlock (the tick waits for a drain that only starts after the tick
//! returns), so `Block` is deliberately not offered here.
//!
//! Only the **default graph** enters the circuit; named-graph writes bypass it
//! (SPEC-29 P2 moves the per-view pipeline onto the circuit).
//!
//! [`HornBackend`]: crate::exec::horn::HornBackend

use horndb_incremental::{ChangeFeedRx, Circuit, DeltaRecord, DerivationKind, LagPolicy, TripleId};
use horndb_storage::{ApplyReport, GraphId, InternedQuad, Store as ColumnStore, TermId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// The reserved graph holding the circuit's derived rows. Inside the SPEC-29
/// reserved namespace (`RESERVED_GRAPH_PREFIX`), so SPARQL Update cannot write
/// it; [`HornBackend::attach_circuit`] admits it to the default union.
///
/// [`HornBackend::attach_circuit`]: crate::exec::horn::HornBackend::attach_circuit
pub const DERIVED_GRAPH: &str = "https://horndb.io/graph/circuit-derived";

/// Records one tick may publish before the feed drops this subscriber and
/// forces a resync. Sized so ordinary updates never resync; a bulk `INSERT
/// DATA` past it costs one O(derived) resync, which is correct, just slower.
pub const FEED_CAPACITY: usize = 1 << 16;

/// The circuit plus this engine's own subscription to its change feed.
pub(crate) struct Wiring {
    /// `Mutex` only to keep this struct `Sync` regardless of what `Circuit`
    /// holds; every access goes through `&mut self`, so it is never contended.
    circuit: Mutex<Circuit>,
    rx: ChangeFeedRx,
    capacity: usize,
    graph: GraphId,
    /// Times the feed dropped this subscriber and the derived graph was
    /// rebuilt from `Circuit::derived_base`.
    pub(crate) resyncs: u64,
}

impl Wiring {
    pub(crate) fn new(
        mut circuit: Circuit,
        store: std::sync::Arc<ColumnStore>,
        graph: GraphId,
        capacity: usize,
    ) -> Self {
        let rx = circuit.subscribe_bounded(capacity, LagPolicy::DisconnectSlow);
        // SPEC-24 S6: `Circuit::snapshot()` reads the same store this wiring
        // writes — the default graph holds the asserted rows, `graph` the
        // derived mirror.
        circuit.attach_store(store, vec![horndb_storage::DEFAULT_GRAPH, graph]);
        Self {
            circuit: Mutex::new(circuit),
            rx,
            capacity,
            graph,
            resyncs: 0,
        }
    }

    pub(crate) fn circuit(&mut self) -> &mut Circuit {
        self.circuit.get_mut().expect("circuit lock poisoned")
    }

    /// One Update operation's worth of base changes: retract, assert, tick,
    /// and return the tick's net derived delta as mirror rows for
    /// [`DERIVED_GRAPH`], without writing them. The caller folds them into
    /// the base batch so the tick is one commit version (ADR-0018). Each row
    /// is an exact flip against `store` (except adds into a `swept` graph,
    /// whose rows the same batch deletes), so the caller can subtract their
    /// counts to recover the base-only report.
    ///
    /// Callers compute `retracts` from storage presence, which also holds
    /// rows bulk-loaded below the funnel; only rows in the asserted base are
    /// retracted, so its multiplicities never go negative (HDB-150).
    ///
    /// Call this *before* committing the batch, and pass the returned
    /// [`Fed`] to [`Self::rollback`] if that commit fails, so the circuit
    /// does not stay a tick ahead of storage.
    pub(crate) fn apply(
        &mut self,
        store: &ColumnStore,
        asserts: &[TripleId],
        retracts: &[TripleId],
        swept: &[GraphId],
    ) -> (Mirror, Fed) {
        let (report, fed) = {
            let c = self.circuit();
            let retracts: Vec<TripleId> = retracts
                .iter()
                .copied()
                .filter(|t| c.asserted_base().get(t) > 0)
                .collect();
            for t in &retracts {
                c.retract_triple(*t);
            }
            for t in asserts {
                c.assert_triple(*t);
            }
            (c.tick(), (asserts.to_vec(), retracts))
        };
        // Durability: the caller commits base and mirror as one logged
        // `Store` batch (SPEC-25 S3), so that one write-ahead-log record is the
        // tick boundary. No circuit input log (`Circuit::attach_input_log`) is
        // attached: replaying the store restores base and derived rows at one
        // consistent version, and reattach re-seeds the circuit from them.
        let published = report.asserted_merged + report.derived_merged;
        let (dels, adds) = match self.drain(published) {
            Some(records) => net_derived(&records),
            None => {
                self.resyncs += 1;
                self.resync(store)
            }
        };
        (self.flips(store, &dels, &adds, swept), fed)
    }

    /// Undo an [`Self::apply`] whose storage commit failed: tick the inverse
    /// of what it fed, then resubscribe so the inverse tick's records (and
    /// any undrained ones) never reach a later `apply`. Z-set ticks are
    /// exactly invertible, so the derived base returns to what storage holds.
    pub(crate) fn rollback(&mut self, (asserts, retracts): Fed) {
        let capacity = self.capacity;
        let c = self.circuit();
        for t in &asserts {
            c.retract_triple(*t);
        }
        for t in &retracts {
            c.assert_triple(*t);
        }
        c.tick();
        self.rx = c.subscribe_bounded(capacity, LagPolicy::DisconnectSlow);
    }

    /// The reserved graph this wiring mirrors derived rows into.
    pub(crate) fn graph(&self) -> GraphId {
        self.graph
    }

    /// Attach-time seed: assert `base` (the default graph as it stands) in
    /// one tick, then make the derived graph equal the circuit's derived base
    /// by diff. Always the diff, never the tick's net: a store reopened from
    /// its WAL still holds the derived rows a *previous* circuit wrote, and
    /// only a want/have diff removes the ones this circuit does not derive.
    pub(crate) fn seed(&mut self, store: &ColumnStore, base: &[TripleId]) -> ApplyReport {
        let c = self.circuit();
        for t in base {
            c.assert_triple(*t);
        }
        c.tick();
        let (dels, adds) = self.resync(store);
        let (dels, adds) = self.flips(store, &dels, &adds, &[]);
        store
            .apply_quad_ids(&dels, &adds)
            .expect("derived-graph seed batch")
    }

    /// The `published` records the tick just put on the feed, or `None` if
    /// the feed dropped this subscriber (`DisconnectSlow`) part-way.
    fn drain(&self, published: usize) -> Option<Vec<DeltaRecord>> {
        let mut out = Vec::with_capacity(published);
        for _ in 0..published {
            // Never blocks: every record was published before `tick()`
            // returned, on this thread. Anything but `Ok` means the
            // subscriber is gone (or the count is wrong); resync either way.
            out.push(self.rx.try_recv().ok()?);
        }
        Some(out)
    }

    /// Resubscribe, then return the `(dels, adds)` diff that makes the
    /// derived graph equal the circuit's derived base (`mult > 0`). Whatever
    /// the old subscription still held is discarded — the base already
    /// includes it.
    fn resync(&mut self, store: &ColumnStore) -> (Vec<TripleId>, Vec<TripleId>) {
        let capacity = self.capacity;
        let (rx, want) = {
            let c = self.circuit();
            let rx = c.subscribe_bounded(capacity, LagPolicy::DisconnectSlow);
            let want: BTreeSet<TripleId> = c
                .derived_base()
                .iter()
                .filter(|(_, m)| *m > 0)
                .map(|(t, _)| *t)
                .collect();
            (rx, want)
        };
        self.rx = rx;
        let have: BTreeSet<TripleId> = store
            .snapshot()
            .iter_graph_term_ids(self.graph)
            .map(|(s, p, o)| (s.0, p.0, o.0))
            .collect();
        let dels: Vec<TripleId> = have.difference(&want).copied().collect();
        let adds: Vec<TripleId> = want.difference(&have).copied().collect();
        (dels, adds)
    }

    /// One net derived delta as derived-graph quads (the ids are already
    /// dictionary ids, so no decode/re-intern round trip), keeping only rows
    /// that change storage: dels that are present, adds that are absent. If
    /// the batch also sweeps the derived graph, every add is kept: the sweep
    /// deletes its present rows first, so presence before says nothing.
    fn flips(
        &self,
        store: &ColumnStore,
        dels: &[TripleId],
        adds: &[TripleId],
        swept: &[GraphId],
    ) -> Mirror {
        let keep_all_adds = swept.contains(&self.graph);
        let dict = store.dictionary();
        let snap = store.snapshot();
        let quad =
            |t: &TripleId| dict.quad_from_ids(self.graph, TermId(t.0), TermId(t.1), TermId(t.2));
        let present =
            |t: &TripleId| snap.contains_quad(self.graph, TermId(t.0), TermId(t.1), TermId(t.2));
        (
            dels.iter().filter(|t| present(t)).map(quad).collect(),
            adds.iter()
                .filter(|t| keep_all_adds || !present(t))
                .map(quad)
                .collect(),
        )
    }
}

/// Derived-graph mirror rows `(dels, adds)` for one tick, each an exact flip.
pub(crate) type Mirror = (Vec<InternedQuad>, Vec<InternedQuad>);

/// The `(asserts, retracts)` one [`Wiring::apply`] actually fed the circuit.
pub(crate) type Fed = (Vec<TripleId>, Vec<TripleId>);

/// Sum a tick's derived records per triple (SPEC-24 S3 consumer rule): a
/// triple with total `> 0` became present, `< 0` became absent, `0` did not
/// change. `Asserted` records are the caller's own writes, already in storage.
fn net_derived(records: &[DeltaRecord]) -> (Vec<TripleId>, Vec<TripleId>) {
    let mut net: BTreeMap<TripleId, i64> = BTreeMap::new();
    for r in records {
        if r.kind != DerivationKind::Asserted {
            *net.entry(r.triple).or_insert(0) += r.mult;
        }
    }
    let mut dels = Vec::new();
    let mut adds = Vec::new();
    for (t, m) in net {
        match m.signum() {
            1 => adds.push(t),
            -1 => dels.push(t),
            _ => {}
        }
    }
    (dels, adds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horndb_incremental::{DerivationKind, RuleId};

    fn rec(triple: TripleId, mult: i64, kind: DerivationKind) -> DeltaRecord {
        DeltaRecord {
            triple,
            mult,
            time: 0,
            kind,
        }
    }

    #[test]
    fn net_sums_per_triple_and_ignores_asserted() {
        let t = (1, 2, 3);
        let u = (4, 5, 6);
        let v = (7, 8, 9);
        let recs = [
            rec(t, 1, DerivationKind::Asserted),
            // ownership move: rule -1, closure +1 => unchanged
            rec(u, -1, DerivationKind::RuleInferred(RuleId::from(1u32))),
            rec(u, 1, DerivationKind::ClosureInferred),
            // withdrawal reaching -2
            rec(v, -1, DerivationKind::RuleInferred(RuleId::from(2u32))),
            rec(v, -1, DerivationKind::ClosureInferred),
            rec((0, 0, 1), 1, DerivationKind::ClosureInferred),
        ];
        let (dels, adds) = net_derived(&recs);
        assert_eq!(dels, vec![v]);
        assert_eq!(adds, vec![(0, 0, 1)]);
    }
}
