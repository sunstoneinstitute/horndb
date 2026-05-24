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
    /// Position into `order` we'll seek next.
    p: usize,
    /// Permutation of indices into `iters` that lists iterators in
    /// non-decreasing key order at prime time. The leapfrog operates
    /// circularly over `order`; sorting on prime is what keeps the
    /// invariant "iter at `order[(p + k - 1) % k]` holds the running max"
    /// after every seek + rotate. See Veldhuizen 2014.
    order: Vec<usize>,
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
            order: Vec::new(),
            done: false,
            primed: false,
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    /// Yield the next value common to all iterators at `depth`, or `None`.
    // Intentionally named `next` for the algorithmic meaning ("next common
    // value") and to match the inlined leapfrog in `executor::wcoj`. We
    // do not implement `Iterator` because the return type is the join's
    // emitted value, not a per-row record.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<TermId> {
        if self.done || self.iters.is_empty() {
            self.done = true;
            return None;
        }

        if !self.primed {
            self.primed = true;
            // Sort iterators by current head so the leapfrog invariant
            // ("iter at `order[prev]` holds the max") holds entering
            // `find_match`. Without this, a priming snapshot like
            // [A=2, B=14, C=2] would falsely report a match of 2 — the
            // loop only compares `iter[p]` against `iter[prev]`, never
            // discovering that B holds a value the others can't reach.
            let mut order: Vec<(usize, TermId)> = Vec::with_capacity(self.iters.len());
            for (i, it) in self.iters.iter().enumerate() {
                match it.peek(self.depth) {
                    None => {
                        self.done = true;
                        return None;
                    }
                    Some(v) => order.push((i, v)),
                }
            }
            order.sort_by_key(|&(_, v)| v);
            self.order = order.into_iter().map(|(i, _)| i).collect();
            self.p = 0;
            return self.find_match();
        }

        // Subsequent call: advance the iterator that just produced the
        // matching value past it, then leapfrog again.
        let k = self.iters.len();
        let cur_iter = self.order[self.p];
        let cur = self.iters[cur_iter].peek(self.depth).unwrap();
        self.iters[cur_iter].seek(self.depth, cur.wrapping_add(1));
        if self.iters[cur_iter].peek(self.depth).is_none() {
            self.done = true;
            return None;
        }
        self.p = (self.p + 1) % k;
        self.find_match()
    }

    /// Core leapfrog loop: advance round-robin until all `k` iterators
    /// agree. Relies on `self.order` listing iterators so that
    /// `iters[order[(p + k - 1) % k]]` always holds the maximum of all
    /// current heads (the invariant is established by sorting on prime
    /// and preserved by each seek making the seeked iter the new max).
    fn find_match(&mut self) -> Option<TermId> {
        let k = self.iters.len();
        loop {
            let prev = (self.p + k - 1) % k;
            let target = self.iters[self.order[prev]].peek(self.depth)?;
            let cur = self.iters[self.order[self.p]].peek(self.depth)?;
            if cur == target {
                return Some(cur);
            }
            self.iters[self.order[self.p]].seek(self.depth, target);
            if self.iters[self.order[self.p]].peek(self.depth).is_none() {
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
            order: Vec::new(),
            done: true,
            primed: true,
        }
    }
}
