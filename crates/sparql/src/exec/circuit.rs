//! SPEC-24 S4 (HDB-51): the DBSP circuit behind [`HornBackend`]'s write funnel.
//!
//! Every SPARQL Update operation lowers to one `assert_triple` /
//! `retract_triple` batch and one `tick()` on the [`Circuit`]. The engine is
//! then its own change-feed subscriber: it drains the records the tick just
//! published, nets them **per triple** (never one record at a time — a tick
//! may carry both a `RuleInferred` and a `ClosureInferred` record for the same
//! triple, and a withdrawal's per-triple total may reach −2), and mirrors the
//! net into the reserved graph [`DERIVED_GRAPH`] with one idempotent tier
//! batch. Derived rows live in their own graph, not the default graph, so a
//! user `DELETE DATA` of a derived triple and an `INSERT DATA` of an
//! already-derived one can never desynchronise the circuit from storage.
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
use horndb_storage::{ApplyReport, GraphId, Store as ColumnStore, TermId};
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
    /// `Mutex` only for `Sync` (`Circuit` holds a `RefCell`); every access
    /// goes through `&mut self`, so it is never contended.
    circuit: Mutex<Circuit>,
    rx: ChangeFeedRx,
    capacity: usize,
    graph: GraphId,
    /// Times the feed dropped this subscriber and the derived graph was
    /// rebuilt from `Circuit::derived_base`.
    pub(crate) resyncs: u64,
}

impl Wiring {
    pub(crate) fn new(circuit: Circuit, graph: GraphId, capacity: usize) -> Self {
        let rx = circuit.subscribe_bounded(capacity, LagPolicy::DisconnectSlow);
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
    /// then mirror the tick's net derived delta into [`DERIVED_GRAPH`].
    /// Returns storage's report for that mirror batch (all zero when nothing
    /// derived changed).
    pub(crate) fn apply(
        &mut self,
        store: &ColumnStore,
        asserts: &[TripleId],
        retracts: &[TripleId],
    ) -> ApplyReport {
        let report = {
            let c = self.circuit();
            for t in retracts {
                c.retract_triple(*t);
            }
            for t in asserts {
                c.assert_triple(*t);
            }
            c.tick()
        };
        // HDB-58 WAL hook: `asserts`/`retracts` are exactly this operation's
        // net base changes, already committed to storage, and the tick's
        // records are on the feed. A durable log append belongs right here,
        // before the derived mirror below.
        let published = report.asserted_merged + report.derived_merged;
        match self.drain(published) {
            Some(records) => {
                let (dels, adds) = net_derived(&records);
                self.write(store, &dels, &adds)
            }
            None => self.resync(store),
        }
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

    /// Resubscribe, then make the derived graph equal the circuit's derived
    /// base (`mult > 0`) with one diff batch. Partial records from the dropped
    /// subscription are discarded — the base already includes them.
    fn resync(&mut self, store: &ColumnStore) -> ApplyReport {
        self.resyncs += 1;
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
        self.write(store, &dels, &adds)
    }

    /// Mirror one net derived delta into the derived graph at the id level
    /// (the ids are already dictionary ids, so no decode/re-intern round
    /// trip). Goes through `Store::apply_quad_ids`, never `Store::tier()`: the
    /// `Store` entry points are where HDB-58's write-ahead log hooks in, and a
    /// tier-level write would bypass it.
    fn write(&self, store: &ColumnStore, dels: &[TripleId], adds: &[TripleId]) -> ApplyReport {
        if dels.is_empty() && adds.is_empty() {
            return ApplyReport::default();
        }
        let dict = store.dictionary();
        let quad =
            |t: &TripleId| dict.quad_from_ids(self.graph, TermId(t.0), TermId(t.1), TermId(t.2));
        let dels: Vec<_> = dels.iter().map(quad).collect();
        let adds: Vec<_> = adds.iter().map(quad).collect();
        store
            .apply_quad_ids(&dels, &adds)
            .expect("derived-graph mirror batch")
    }
}

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
