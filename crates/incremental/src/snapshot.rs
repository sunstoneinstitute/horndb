//! SPEC-06 F7 — in-flight reader visibility via MVCC snapshots.
//!
//! A [`Snapshot`] pins the materialized `(asserted ∪ derived)` **set** of
//! triples present in a [`crate::circuit::Circuit`] at the logical time it was
//! acquired. It is refcount-backed: acquiring clones an `Arc` (O(1)), and the
//! pinned version is immutable, so subsequent `tick()`s that publish a *new*
//! version leave this handle's view untouched until it is dropped. Readers
//! (snapshot holders) and writers (`tick`) therefore never block each other.
//!
//! ## Set semantics (presence), not Z-set multiplicity
//!
//! This is deliberately a **presence/set view**: a triple is either present or
//! absent, never present "twice". The underlying engine is a Z-set (records
//! carry `±1` multiplicities so signed deltas net out — a triple asserted then
//! retracted is absent), but a reader-facing snapshot of an RDF graph is a
//! *set* of triples — `(asserted ∪ derived)` is a union, and the rest of the
//! store queries presence (`get(t) != 0`). Exposing a raw multiplicity (e.g.
//! `2` for a triple that is both derived and re-asserted, or asserted twice)
//! would be meaningless to an RDF consumer, so the snapshot collapses to
//! presence on publish and the query surface here is presence-only. Point
//! queries against partially-applied in-flight deltas mid-tick stay deferred
//! under parent #6.
//!
//! Scope (issue #46): in-flight reader visibility within `horndb-incremental`.
//! Full SPEC-02 per-tuple storage MVCC stays deferred under parent #6.

use std::sync::Arc;

use crate::types::{LogicalTime, TripleId};
use crate::zset::Zset;

/// A consistent, refcounted **set** of the triples present in a
/// [`Circuit`](crate::circuit::Circuit)'s materialized `(asserted ∪ derived)`
/// view at a fixed logical time. Cheap to clone (Arc bump). Presence-only — see
/// the [module docs](self) for why this is a set view rather than a Z-set.
#[derive(Clone, Debug)]
pub struct Snapshot {
    time: LogicalTime,
    view: Arc<Zset<TripleId>>,
}

impl Snapshot {
    /// Construct a snapshot over an already-materialized version. Internal:
    /// callers go through [`Circuit::snapshot`](crate::circuit::Circuit::snapshot).
    /// The `view` must already be collapsed to presence (every present triple at
    /// multiplicity 1); `Circuit::tick` builds it that way.
    pub(crate) fn new(time: LogicalTime, view: Arc<Zset<TripleId>>) -> Self {
        Self { time, view }
    }

    /// The logical time this snapshot represents: it reflects every asserted
    /// record with timestamp ≤ this value (SPEC-06 F7).
    pub fn logical_time(&self) -> LogicalTime {
        self.time
    }

    /// Whether `triple` is present in the pinned set.
    pub fn contains(&self, triple: &TripleId) -> bool {
        self.view.get(triple) != 0
    }

    /// Number of distinct triples present in the pinned set.
    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// Whether the pinned set holds no triples.
    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Iterate the triples present in the pinned set (presence view — each
    /// present triple is yielded exactly once).
    pub fn iter(&self) -> impl Iterator<Item = &TripleId> {
        self.view.iter().map(|(triple, _)| triple)
    }
}
