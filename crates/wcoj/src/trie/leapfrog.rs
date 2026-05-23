//! Leapfrog single-variable intersection.
//!
//! Given `k` trie iterators all positioned at the same variable depth, the
//! leapfrog algorithm advances them in round-robin until either (a) all
//! `k` iterators agree on a value (emit it) or (b) one of them runs off the
//! end (terminate). This is the inner loop of Veldhuizen's triejoin and
//! contributes the per-tuple cost we're trying to keep ≤5 ns/tuple.

use crate::ids::TermId;
use crate::trie::TrieIterator;

pub struct LeapfrogJoin<'a> {
    iters: Vec<Box<dyn TrieIterator + 'a>>,
    depth: u8,
    /// Position into `iters` we'll seek next.
    p: usize,
    /// True once we know the join is exhausted at this depth.
    done: bool,
    /// True before the first call to `next` — we don't seek on the very
    /// first call, just check the current heads.
    primed: bool,
}

impl<'a> LeapfrogJoin<'a> {
    pub fn new(iters: Vec<Box<dyn TrieIterator + 'a>>, depth: u8) -> Self {
        Self {
            iters,
            depth,
            p: 0,
            done: false,
            primed: false,
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    /// Yield the next value common to all iterators at `depth`, or `None`.
    pub fn next(&mut self) -> Option<TermId> {
        if self.done || self.iters.is_empty() {
            self.done = true;
            return None;
        }

        if !self.primed {
            self.primed = true;
            // Sort iterators by current head so we can leapfrog deterministically.
            // For correctness we don't need to sort — but sorting picks the
            // smallest max-min gap first, which is faster.
            self.iters
                .sort_by_key(|it| it.peek(self.depth).unwrap_or(TermId::MAX));
            if self.iters.iter().any(|it| it.peek(self.depth).is_none()) {
                self.done = true;
                return None;
            }
            // After sort, p starts at 0 and the target is iters[k-1].peek.
            self.p = 0;
            return self.find_match();
        }

        // Subsequent call: advance the iterator that just produced the
        // matching value past it, then leapfrog again.
        let k = self.iters.len();
        // The matching value was iters[p].peek; advance past it.
        let cur = self.iters[self.p].peek(self.depth).unwrap();
        self.iters[self.p].seek(self.depth, cur.wrapping_add(1));
        if self.iters[self.p].peek(self.depth).is_none() {
            self.done = true;
            return None;
        }
        self.p = (self.p + 1) % k;
        self.find_match()
    }

    /// Core leapfrog loop: advance round-robin until all `k` iterators
    /// agree.
    fn find_match(&mut self) -> Option<TermId> {
        let k = self.iters.len();
        loop {
            // The target is the largest current head; we seek the iterator
            // at position `p` to it.
            let prev = (self.p + k - 1) % k;
            let target = self.iters[prev].peek(self.depth)?;
            let cur = self.iters[self.p].peek(self.depth)?;
            if cur == target {
                return Some(cur);
            }
            self.iters[self.p].seek(self.depth, target);
            if self.iters[self.p].peek(self.depth).is_none() {
                self.done = true;
                return None;
            }
            self.p = (self.p + 1) % k;
        }
    }

    /// Mutable access to the held iterators (used by the WCOJ executor when
    /// descending to call `open_level` on each contributing iter).
    pub fn iters_mut(&mut self) -> &mut [Box<dyn TrieIterator + 'a>] {
        &mut self.iters
    }

    /// Consume the join, returning the held iterators (used by the WCOJ
    /// executor when ascending).
    pub fn into_iters(self) -> Vec<Box<dyn TrieIterator + 'a>> {
        self.iters
    }

    /// Construct an "already-entered" marker that holds no iterators. Used
    /// by the executor to remember it has descended past this depth.
    pub fn reentry_marker(depth: u8) -> Self {
        Self {
            iters: Vec::new(),
            depth,
            p: 0,
            done: true,
            primed: true,
        }
    }
}
