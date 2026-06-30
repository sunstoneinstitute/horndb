//! Storage abstraction over which the WCOJ executor operates.
//!
//! SPEC-02 (`horndb-storage`) will provide the production implementation;
//! the executor never depends on the storage crate directly. Instead, anything
//! that can serve sorted iterators with `seek` over one of the six orderings
//! implements `TripleSource`.

pub mod compressed;
pub mod packed_column;
pub(crate) mod soa;
pub mod synthetic;
pub mod vec_source;

use crate::error::Result;
use crate::ids::{Ordering, TermId};

/// A multi-ordering RDF triple source.
///
/// The associated [`Self::Iter`] is intentionally a generic associated
/// type so the executor's hot path can be monomorphised against the
/// concrete iter (no `Box<dyn>` indirection). This makes the trait
/// non-object-safe — `&dyn TripleSource` no longer compiles; pass it as
/// a generic bound (`<S: TripleSource>`) instead. The executor and the
/// cardinality estimator are both already generic over the source type.
pub trait TripleSource: Send + Sync {
    /// The iterator type returned by [`Self::iter`]. Concrete (not boxed)
    /// so peek/seek calls on the leapfrog hot path inline.
    type Iter<'a>: OrderedTripleIter + 'a
    where
        Self: 'a;

    /// Open a new cursor over the triples in `ord` ordering.
    fn iter(&self, ord: Ordering) -> Result<Self::Iter<'_>>;

    /// Total triple count across all predicates. Used by the cardinality stub.
    fn total_triples(&self) -> usize;

    /// True if `ord` is materialised; false if the executor must ask for a
    /// different ordering. Stage-1 implementations may return true for all six.
    fn supports(&self, ord: Ordering) -> bool {
        let _ = ord;
        true
    }
}

/// A trie-shaped, depth-aware cursor over triples in some [`Ordering`].
///
/// The cursor maintains an implicit "current path" — values chosen at each
/// upper level constrain what is visible at deeper levels. `peek(depth)`
/// returns the next value at `depth` consistent with the prefix; `seek(depth,
/// v)` advances to the first value ≥ `v` at that depth; `open_level(depth)`
/// descends one level (must have peeked a value at `depth - 1` first).
pub trait OrderedTripleIter: Send {
    /// Return the next value at `depth` consistent with the current prefix,
    /// or `None` if the cursor is past the end at this level.
    fn peek(&self, depth: u8) -> Option<TermId>;

    /// Seek forward at `depth` to the first value ≥ `value`. After this call,
    /// `peek(depth)` returns either that value or `None`.
    fn seek(&mut self, depth: u8, value: TermId);

    /// Descend into the subtree under the value most recently peeked at
    /// `depth - 1`. Implementations may panic if the prefix is empty.
    fn open_level(&mut self, depth: u8);

    /// Ascend one level, undoing the matching `open_level`.
    fn up(&mut self, depth: u8);

    /// Rewind the cursor at `depth` to the start of the current subtree
    /// (the `lo` of the range last established by `open_level(depth)` or,
    /// for `depth == 0`, the full data range). Used by the executor on
    /// carry-iter refresh during ascent re-entry: it positions the iter
    /// at the first value under the current ancestor binding without the
    /// up+open_level round-trip that recomputes the same range.
    ///
    /// Precondition: `open_level(depth)` (or root for `depth == 0`) was
    /// last called and not subsequently undone by `up(depth)`. The
    /// default impl falls back to up+open_level, which is correct but
    /// allocates no faster than a fresh descent — concrete sources should
    /// override.
    fn rewind(&mut self, depth: u8) {
        if depth == 0 {
            self.up(0);
        } else {
            self.up(depth);
            self.open_level(depth);
        }
    }

    /// True if the cursor has exhausted values at `depth`.
    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }

    /// If this cursor can cheaply expose its active level's remaining values
    /// (from the current cursor to the level end) as a contiguous,
    /// non-decreasing `&[TermId]`, return it — for the leapfrog SIMD-intersect
    /// fast path. The slice is sorted but **not** deduplicated: the raw level
    /// column repeats a key once per child row, so callers that need distinct
    /// values must dedup it. Default `None`. Takes `&mut self` because a source
    /// may need to materialise the contiguous view on demand (e.g. the dense
    /// AoS `VecIter` builds its SoA column lazily).
    fn active_run(&mut self, _depth: u8) -> Option<&[TermId]> {
        None
    }
}
