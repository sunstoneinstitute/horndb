//! Storage abstraction over which the WCOJ executor operates.
//!
//! SPEC-02 (`reasoner-storage`) will provide the production implementation;
//! the executor never depends on the storage crate directly. Instead, anything
//! that can serve sorted iterators with `seek` over one of the six orderings
//! implements `TripleSource`.

pub mod synthetic;
pub mod vec_source;

use crate::error::Result;
use crate::ids::{Ordering, TermId};

/// A multi-ordering RDF triple source.
pub trait TripleSource: Send + Sync {
    /// The iterator type returned by [`Self::iter`]. Boxed to keep the trait
    /// object-safe; Stage-2 may revisit if dispatch cost shows up in profiles.
    fn iter(&self, ord: Ordering) -> Result<Box<dyn OrderedTripleIter + '_>>;

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

    /// True if the cursor has exhausted values at `depth`.
    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }
}
