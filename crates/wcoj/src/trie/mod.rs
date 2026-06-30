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

    /// Refresh the cursor at `global_depth` for the *current* ancestor
    /// binding context, undoing any local-depth-greater-than-`global_depth`
    /// advances. Used when re-entering depth `d` for an iter whose top
    /// contribution is at some shallower depth — it needs to re-explore
    /// from the start of its subtree under the current parent state.
    fn refresh(&mut self, _global_depth: u8) {}

    fn at_end(&self, depth: u8) -> bool {
        self.peek(depth).is_none()
    }

    /// If this iterator can expose its active level's remaining values as a
    /// contiguous sorted `&[TermId]`, return it (for the leapfrog SIMD
    /// intersect fast path). Default `None` — the leapfrog falls back to
    /// seek/peek. The slice runs from the current cursor to the level end, in
    /// non-decreasing trie order. It is **not** deduplicated: when this
    /// pattern descends below the level, the underlying column repeats each key
    /// once per child row, so a key can appear multiple times. Callers that
    /// need each distinct value once (e.g. feeding `horndb_simd::intersect`,
    /// whose contract requires deduped inputs) must dedup it themselves — see
    /// `executor::wcoj::BatchIter::try_arm_simd`. Takes `&mut self` so a source
    /// can materialise the view on demand.
    fn active_run(&mut self, _depth: u8) -> Option<&[TermId]> {
        None
    }
}
