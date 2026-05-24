//! Trie iterators and the per-variable leapfrog seek loop.
//!
//! A [`TrieIterator`] is a depth-aware cursor; it differs from an
//! [`OrderedTripleIter`] only in that depths refer to *query variables* in a
//! fixed variable ordering, not to physical SPO positions. One
//! [`TrieIterator`] is produced per triple pattern; the leapfrog algorithm
//! intersects them at each variable level.
//!
//! See Veldhuizen, *Leapfrog Triejoin: a worst-case optimal join algorithm*,
//! ICDT 2014.

pub mod leapfrog;
pub mod source_iter;

use crate::ids::TermId;

pub trait TrieIterator {
    /// Number of variable levels in this iterator. The trie operates on
    /// `0..arity()`.
    fn arity(&self) -> u8;

    fn peek(&self, depth: u8) -> Option<TermId>;
    fn seek(&mut self, depth: u8, value: TermId);
    fn open_level(&mut self, depth: u8);
    fn up(&mut self, depth: u8);

    /// Reset the iter to its post-construction state. Used by the executor
    /// when re-entering a depth that this iter's top-contribution depth
    /// equals (so its cursor advance must be undone).
    fn reset(&mut self) {}

    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }
}
