//! Newtype IDs used across the closure backend.
//!
//! `DictId` is a dictionary-encoded term ID from SPEC-02 (storage). We treat
//! it as opaque here — closure does not know how to decode URIs, only how to
//! count and renumber them. `DenseIdx` is a 0-based row/column index inside
//! a single predicate's renumbered matrix.

/// Dictionary-encoded term ID from SPEC-02. Stable across the lifetime of the
/// store; closure never invents new ones.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct DictId(pub u64);

/// Dense per-predicate row/column index. Local to one matrix; do not mix
/// indices from different predicates.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct DenseIdx(pub u64);

/// Dictionary ID of a predicate.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
#[repr(transparent)]
pub struct PredicateId(pub u64);

/// A subject/object pair within one predicate's extent.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Edge {
    pub s: DictId,
    pub o: DictId,
}

/// A full triple in dictionary IDs.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Triple {
    pub s: DictId,
    pub p: PredicateId,
    pub o: DictId,
}
