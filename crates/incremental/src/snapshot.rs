//! SPEC-06 F7 — in-flight reader visibility via MVCC snapshots.
//!
//! A [`Snapshot`] pins the materialized `(asserted ∪ derived)` Z-set view of a
//! [`crate::circuit::Circuit`] at the logical time it was acquired. It is
//! refcount-backed: acquiring clones an `Arc` (O(1)), and the pinned version is
//! immutable, so subsequent `tick()`s that publish a *new* version leave this
//! handle's view untouched until it is dropped. Readers (snapshot holders) and
//! writers (`tick`) therefore never block each other.
//!
//! Scope (issue #46): in-flight reader visibility within `horndb-incremental`.
//! Full SPEC-02 per-tuple storage MVCC stays deferred under parent #6.

use std::sync::Arc;

use crate::types::{LogicalTime, Multiplicity, TripleId};
use crate::zset::Zset;

/// A consistent, refcounted view of a [`Circuit`](crate::circuit::Circuit)'s
/// materialized state at a fixed logical time. Cheap to clone (Arc bump).
#[derive(Clone, Debug)]
pub struct Snapshot {
    time: LogicalTime,
    view: Arc<Zset<TripleId>>,
}

impl Snapshot {
    /// Construct a snapshot over an already-materialized version. Internal:
    /// callers go through [`Circuit::snapshot`](crate::circuit::Circuit::snapshot).
    pub(crate) fn new(time: LogicalTime, view: Arc<Zset<TripleId>>) -> Self {
        Self { time, view }
    }

    /// The logical time this snapshot represents: it reflects every asserted
    /// record with timestamp ≤ this value (SPEC-06 F7).
    pub fn logical_time(&self) -> LogicalTime {
        self.time
    }

    /// Multiplicity of `triple` in the pinned view (0 if absent).
    pub fn get(&self, triple: &TripleId) -> Multiplicity {
        self.view.get(triple)
    }

    /// Whether `triple` is present (non-zero multiplicity) in the pinned view.
    pub fn contains(&self, triple: &TripleId) -> bool {
        self.view.get(triple) != 0
    }

    /// Number of distinct triples in the pinned view.
    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// Whether the pinned view holds no triples.
    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Iterate `(triple, multiplicity)` pairs of the pinned view.
    pub fn iter(&self) -> impl Iterator<Item = (&TripleId, Multiplicity)> {
        self.view.iter()
    }
}
