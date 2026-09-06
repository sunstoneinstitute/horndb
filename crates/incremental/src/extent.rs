//! `PredExtent` — the base extent indexed by predicate (SPEC-24 S7).
//!
//! `NaryPlan` binds each join leaf to the slice of the base extent holding
//! only that leaf's predicate, instead of scanning every asserted and
//! derived triple on every leaf regardless of predicate. A rule whose body
//! pattern has a variable in predicate position (no fixed leaf predicate,
//! e.g. prp-dom's `(?x ?p ?y)`) still reads the whole extent via `all()`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::types::{Multiplicity, TripleId};
use crate::zset::Zset;

static EMPTY: LazyLock<Zset<TripleId>> = LazyLock::new(Zset::new);

/// The base extent, held both as one flat Z-set (`all`) and as per-predicate
/// slices (`by_pred`). Every write goes to both, so they never disagree.
///
/// ponytail: `all` duplicates every row already held in `by_pred`, doubling
/// extent memory. Drop it once variable-predicate leaves (rules whose
/// `body_predicates()` is `None` for a side) probe `by_pred` directly
/// instead of needing one flat view.
#[derive(Default)]
pub struct PredExtent {
    by_pred: BTreeMap<u64, Zset<TripleId>>,
    all: Zset<TripleId>,
}

impl PredExtent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a `PredExtent` from a flat Z-set, indexing every row by its
    /// predicate.
    pub fn from_zset(z: &Zset<TripleId>) -> Self {
        let mut ext = Self::new();
        ext.add_assign(z);
        ext
    }

    /// The slice of rows whose predicate is `p`. A shared empty `Zset` when
    /// `p` has no rows.
    pub fn slice(&self, p: u64) -> &Zset<TripleId> {
        self.by_pred.get(&p).unwrap_or(&EMPTY)
    }

    /// The whole extent, across every predicate.
    pub fn all(&self) -> &Zset<TripleId> {
        &self.all
    }

    pub fn get(&self, t: &TripleId) -> Multiplicity {
        self.all.get(t)
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    /// Iterate `(&TripleId, Multiplicity)` pairs in key order (delegates to
    /// `all`).
    pub fn iter(&self) -> impl Iterator<Item = (&TripleId, Multiplicity)> {
        self.all.iter()
    }

    /// Add `delta` to the multiplicity of `t`, in both `all` and its
    /// predicate slice. Removes the row (from both) if the resulting
    /// multiplicity is zero.
    pub fn add(&mut self, t: TripleId, delta: Multiplicity) {
        if delta == 0 {
            return;
        }
        self.all.add(t, delta);
        let (_, p, _) = t;
        let sub = self.by_pred.entry(p).or_default();
        sub.add(t, delta);
        if sub.is_empty() {
            self.by_pred.remove(&p);
        }
    }

    /// Pointwise sum: `self += other`.
    pub fn add_assign(&mut self, other: &Zset<TripleId>) {
        for (t, m) in other.iter() {
            self.add(*t, m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_is_subset_of_all() {
        let z = Zset::from_iter([((1, 10, 2), 1), ((2, 10, 3), 1), ((5, 20, 6), 1)]);
        let ext = PredExtent::from_zset(&z);

        for (t, m) in ext.slice(10).iter() {
            assert_eq!(
                ext.all().get(t),
                m,
                "slice(10) row {t:?} disagrees with all()"
            );
        }
        assert_eq!(ext.slice(10).len(), 2);
        assert_eq!(ext.slice(20).len(), 1);
        // A predicate with no rows returns a (shared) empty slice.
        assert!(ext.slice(999).is_empty());
    }

    #[test]
    fn add_negative_removes_from_both_maps() {
        let mut ext = PredExtent::new();
        ext.add((1, 10, 2), 1);
        assert_eq!(ext.get(&(1, 10, 2)), 1);
        assert_eq!(ext.slice(10).get(&(1, 10, 2)), 1);

        ext.add((1, 10, 2), -1);
        assert_eq!(ext.get(&(1, 10, 2)), 0);
        assert_eq!(ext.slice(10).get(&(1, 10, 2)), 0);
        assert!(ext.slice(10).is_empty());
        assert!(ext.is_empty());
    }

    #[test]
    fn from_zset_round_trips_through_iter() {
        let z = Zset::from_iter([((1, 10, 2), 1), ((2, 10, 3), 3), ((5, 20, 6), 1)]);
        let ext = PredExtent::from_zset(&z);
        let round_tripped = Zset::from_iter(ext.iter().map(|(t, m)| (*t, m)));
        assert_eq!(round_tripped, z);
    }
}
