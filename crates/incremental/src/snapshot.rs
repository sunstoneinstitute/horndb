//! SPEC-24 S6 — reader snapshots backed by SPEC-02/SPEC-25 per-tuple MVCC.
//!
//! A [`Snapshot`] is a pinned, read-only view of the storage tier that holds a
//! [`Circuit`](crate::circuit::Circuit)'s materialized `(asserted ∪ derived)`
//! triples — the default graph plus the circuit's derived graph. Acquiring one
//! pins the tier's current commit version (an `Arc` clone plus a pin-count
//! bump), so it is O(1) and never rebuilds a presence set in the circuit.
//! Storage's copy-on-write tier leaves a pinned version untouched while later
//! ticks commit newer ones, so readers and writers never block each other.
//!
//! ## One clock (ADR-0018)
//!
//! `logical_time()` is the storage **commit version** the view is pinned to,
//! not a separate circuit counter. "Snapshot at t" therefore means the same
//! thing in the circuit and in storage, with no mapping to persist. Version
//! `0` is the empty store; the first commit is version `1`. The token is
//! inclusive: the view shows every tuple whose visibility stamp range covers
//! that version.
//!
//! ## Set semantics (presence), not Z-set multiplicity
//!
//! Storage rows are present or absent, never present "twice", so this view is
//! a set. A triple that is both asserted and derived lives in two graphs and
//! is still yielded once — [`Snapshot::iter`] merges the graphs and dedupes.

use std::sync::Arc;

use horndb_storage::{GraphId, PinnedSnapshot, TermId};

use crate::types::{LogicalTime, TripleId};

/// A pinned, MVCC-consistent **set** of the triples a
/// [`Circuit`](crate::circuit::Circuit) has materialized in storage, as of one
/// commit version. Cheap to clone (two `Arc` bumps).
#[derive(Clone)]
pub struct Snapshot {
    pin: Arc<PinnedSnapshot>,
    /// The graphs whose union is the circuit's view, in the order given to
    /// [`Circuit::attach_store`](crate::circuit::Circuit::attach_store).
    graphs: Arc<[GraphId]>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("logical_time", &self.logical_time())
            .field("graphs", &&*self.graphs)
            .finish()
    }
}

impl Snapshot {
    /// Pin `store` at its current commit version. Internal: callers go through
    /// [`Circuit::snapshot`](crate::circuit::Circuit::snapshot).
    pub(crate) fn new(pin: PinnedSnapshot, graphs: Arc<[GraphId]>) -> Self {
        Self {
            pin: Arc::new(pin),
            graphs,
        }
    }

    /// The **inclusive** as-of token: the storage commit version this view is
    /// pinned to (ADR-0018 — the engine's one logical clock). Monotonically
    /// non-decreasing across ticks; `0` is the empty store.
    pub fn logical_time(&self) -> LogicalTime {
        self.pin.version()
    }

    /// Whether `triple` is visible in any of the view's graphs at the pinned
    /// version. O(log rows) per graph — a binary search in the predicate
    /// partition, not a scan.
    pub fn contains(&self, triple: &TripleId) -> bool {
        let at = self.pin.version();
        let (s, p, o) = (TermId(triple.0), TermId(triple.1), TermId(triple.2));
        self.graphs.iter().any(|g| {
            self.pin
                .with_predicate(*g, p, |part| part.contains_at(s, o, at))
                .unwrap_or(false)
        })
    }

    /// Number of distinct triples visible in the pinned view. O(visible rows):
    /// the graphs can overlap (a triple both asserted and derived), so this
    /// counts the deduped merge rather than summing per-graph row counts.
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether the pinned view holds no triples.
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// Key-ordered iteration over the visible triples: predicate id ascending,
    /// then `(subject, object)` ascending within each predicate — storage's
    /// own key order. Each distinct triple is yielded exactly once even when
    /// it is present in more than one of the view's graphs.
    ///
    /// One predicate's rows are materialized at a time (to merge the graphs),
    /// never the whole view.
    pub fn iter(&self) -> impl Iterator<Item = TripleId> + '_ {
        let at = self.pin.version();
        let mut predicates: Vec<TermId> = self
            .graphs
            .iter()
            .flat_map(|g| self.pin.predicates(*g))
            .collect();
        predicates.sort_unstable_by_key(|p| p.0);
        predicates.dedup();
        predicates.into_iter().flat_map(move |p| {
            let mut rows: Vec<TripleId> = self
                .graphs
                .iter()
                .filter_map(|g| {
                    self.pin.with_predicate(*g, p, |part| {
                        part.scan_at(at)
                            .map(|(s, o)| (s.0, p.0, o.0))
                            .collect::<Vec<_>>()
                    })
                })
                .flatten()
                .collect();
            rows.sort_unstable();
            rows.dedup();
            rows
        })
    }
}
