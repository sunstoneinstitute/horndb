//! `MergedIter` — a trie cursor that merges several already-sorted **leaf
//! blocks** instead of one flat index.
//!
//! A leaf block is a set of triples that share one component — in HornDB, one
//! predicate partition — stored as the other two components in two sorted
//! columns. That is exactly the shape `horndb-storage` keeps on disk/in memory,
//! so a caller can hand its stored columns straight to this cursor and skip
//! building a [`crate::source::vec_source::VecTripleSource`] copy of the store
//! per query (HDB-120).
//!
//! # Why one leaf serves every ordering
//!
//! Inside a leaf the shared component is constant, so the leaf's rows are
//! *already* in every ordering's component order — only the depth at which the
//! constant sits changes:
//!
//! | Ordering | depth 0 | depth 1 | depth 2 | axis columns (`level0`, `level1`) |
//! |---|---|---|---|---|
//! | `Pso` | key (p) | col0 | col1 | (subject, object) |
//! | `Pos` | key (p) | col0 | col1 | (object, subject) |
//! | `Spo` | col0 | key (p) | col1 | (subject, object) |
//! | `Ops` | col0 | key (p) | col1 | (object, subject) |
//! | `Sop` | col0 | col1 | key (p) | (subject, object) |
//! | `Osp` | col0 | col1 | key (p) | (object, subject) |
//!
//! So the caller only has to pick the right *axis* — subject-major for `Spo`,
//! `Sop`, `Pso`; object-major for `Pos`, `Osp`, `Ops` — and this cursor places
//! the constant at the right depth itself.
//!
//! # What merging costs
//!
//! Every level operation is a linear pass over the leaves still live at that
//! depth, against `VecIter`'s single binary search over one flat column. With
//! one leaf per predicate that is a small constant at the depths where the key
//! leads (`Pso`/`Pos`: one live leaf below depth 0), and O(predicates) at
//! depth 0 of a subject- or object-leading ordering. The trade is the whole
//! point: no per-query sorted copy of the store.

use crate::ids::{Ordering, TermId};
use crate::source::OrderedTripleIter;

/// One sorted leaf block: every row shares `key`, and `level0`/`level1` hold
/// the other two components.
///
/// Contract, unchecked in release (see [`MergedIter::new`]'s `debug_assert`s):
/// * `level0` and `level1` have the same length,
/// * rows are sorted ascending by `(level0[i], level1[i])` with no duplicate
///   pair, and
/// * the columns are the axis columns for the ordering the cursor is opened
///   in (subject-major or object-major — see the module table).
#[derive(Clone, Copy)]
pub struct MergedLeaf<'a> {
    pub key: TermId,
    pub level0: &'a [TermId],
    pub level1: &'a [TermId],
}

impl<'a> MergedLeaf<'a> {
    #[inline]
    fn col(&self, c: usize) -> &'a [TermId] {
        if c == 0 {
            self.level0
        } else {
            self.level1
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.level0.len()
    }
}

/// What a trie depth reads from a leaf.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    /// The leaf's constant component. Every row in the leaf's current range
    /// carries it, so the depth holds one value.
    Key,
    /// Column 0 or 1 of the leaf, indexed by the leaf's cursor.
    Col(usize),
}

/// Where `ord` puts the leaf's constant component.
const fn key_depth(ord: Ordering) -> usize {
    match ord {
        Ordering::Pso | Ordering::Pos => 0,
        Ordering::Spo | Ordering::Ops => 1,
        Ordering::Sop | Ordering::Osp => 2,
    }
}

fn slots(ord: Ordering) -> [Slot; 3] {
    let kd = key_depth(ord);
    let mut out = [Slot::Key; 3];
    let mut next_col = 0usize;
    for (d, slot) in out.iter_mut().enumerate() {
        if d != kd {
            *slot = Slot::Col(next_col);
            next_col += 1;
        }
    }
    out
}

/// One leaf's row window at one depth. `cur` is the next row to read; `cur ==
/// hi` means the leaf is exhausted at this depth. For a [`Slot::Key`] depth
/// the whole window carries the one key value, so `cur` only ever holds `lo`
/// (live) or `hi` (consumed).
#[derive(Clone, Copy, Default)]
struct Window {
    lo: usize,
    hi: usize,
    cur: usize,
}

/// Trie cursor over a set of [`MergedLeaf`]s in one [`Ordering`].
pub struct MergedIter<'a> {
    leaves: Vec<MergedLeaf<'a>>,
    slots: [Slot; 3],
    /// Per depth, per leaf (indexed by position in `leaves`).
    win: [Vec<Window>; 3],
    /// Per depth, the leaves whose prefix still matches — indices into
    /// `leaves`, kept in ascending order so a `Key` depth's values stay sorted.
    live: [Vec<u32>; 3],
}

impl<'a> MergedIter<'a> {
    /// Open a cursor over `leaves` in `ord`.
    ///
    /// `leaves` must be sorted by `key` with distinct keys — the caller has
    /// them keyed by predicate, so that is a sort of the predicate list.
    /// Empty leaves are dropped.
    pub fn new(ord: Ordering, mut leaves: Vec<MergedLeaf<'a>>) -> Self {
        leaves.retain(|l| l.len() > 0);
        leaves.sort_by_key(|l| l.key);
        debug_assert!(
            leaves.windows(2).all(|w| w[0].key < w[1].key),
            "merged leaves must have distinct keys"
        );
        debug_assert!(
            leaves.iter().all(|l| l.level0.len() == l.level1.len()),
            "a leaf's two columns must have the same length"
        );
        let n = leaves.len();
        let root: Vec<Window> = leaves
            .iter()
            .map(|l| Window {
                lo: 0,
                hi: l.len(),
                cur: 0,
            })
            .collect();
        Self {
            slots: slots(ord),
            win: [root, vec![Window::default(); n], vec![Window::default(); n]],
            live: [(0..n as u32).collect(), Vec::new(), Vec::new()],
            leaves,
        }
    }

    /// Total rows across all leaves. Leaves are deduplicated blocks over
    /// disjoint keys, so this is the triple count.
    pub fn total_rows(leaves: &[MergedLeaf<'_>]) -> usize {
        leaves.iter().map(|l| l.len()).sum()
    }

    /// Leaf `i`'s value at `depth`, or `None` if it is exhausted there.
    #[inline]
    fn value_at(&self, depth: usize, i: usize) -> Option<TermId> {
        let w = self.win[depth][i];
        if w.cur >= w.hi {
            return None;
        }
        Some(match self.slots[depth] {
            Slot::Key => self.leaves[i].key,
            Slot::Col(c) => self.leaves[i].col(c)[w.cur],
        })
    }
}

impl OrderedTripleIter for MergedIter<'_> {
    fn peek(&self, depth: u8) -> Option<TermId> {
        let d = depth as usize;
        // ponytail: linear min over the live leaves. One leaf per predicate,
        // and every depth below a `Key` depth has exactly one live leaf, so
        // the scan is short except at depth 0 of a subject-/object-leading
        // ordering. Swap in a loser tree if a profile says that depth hurts.
        self.live[d]
            .iter()
            .filter_map(|&i| self.value_at(d, i as usize))
            .min()
    }

    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let slot = self.slots[d];
        let leaves = &self.leaves;
        let win = &mut self.win[d];
        for &i in &self.live[d] {
            let i = i as usize;
            let w = &mut win[i];
            if w.cur >= w.hi {
                continue;
            }
            match slot {
                Slot::Key => {
                    if leaves[i].key < value {
                        w.cur = w.hi;
                    }
                }
                Slot::Col(c) => {
                    let col = &leaves[i].col(c)[w.cur..w.hi];
                    w.cur += col.partition_point(|&x| x < value);
                }
            }
        }
    }

    fn open_level(&mut self, depth: u8) {
        assert!((1..=2).contains(&depth), "open_level depth must be 1 or 2");
        let d = depth as usize;
        let parent = d - 1;
        self.live[d].clear();
        // No value at the parent — nothing to descend into. `VecIter` may
        // panic here; staying empty is the same observable state (`peek`
        // returns `None`) and keeps the "bound level missed" path in
        // `PatternTrieIter` cheap.
        let Some(v) = self.peek(depth - 1) else {
            for w in self.win[d].iter_mut() {
                *w = Window::default();
            }
            return;
        };
        for k in 0..self.live[parent].len() {
            let i = self.live[parent][k] as usize;
            if self.value_at(parent, i) != Some(v) {
                self.win[d][i] = Window::default();
                continue;
            }
            let pw = self.win[parent][i];
            let (lo, hi) = match self.slots[parent] {
                // The key spans the whole parent window.
                Slot::Key => (pw.cur, pw.hi),
                // Rows are sorted and the cursor sits on the first row equal
                // to `v`, so that run is a prefix of the parent window's tail.
                Slot::Col(c) => {
                    let col = &self.leaves[i].col(c)[pw.cur..pw.hi];
                    (pw.cur, pw.cur + col.partition_point(|&x| x == v))
                }
            };
            self.win[d][i] = Window { lo, hi, cur: lo };
            self.live[d].push(i as u32);
        }
    }

    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            for (i, w) in self.win[0].iter_mut().enumerate() {
                *w = Window {
                    lo: 0,
                    hi: self.leaves[i].len(),
                    cur: 0,
                };
            }
            self.live[0].clear();
            self.live[0].extend(0..self.leaves.len() as u32);
        } else {
            for &i in &self.live[d] {
                self.win[d][i as usize] = Window::default();
            }
            self.live[d].clear();
        }
    }

    fn rewind(&mut self, depth: u8) {
        let d = depth as usize;
        for &i in &self.live[d] {
            let w = &mut self.win[d][i as usize];
            w.cur = w.lo;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Triple;
    use crate::source::vec_source::VecTripleSource;
    use crate::source::TripleSource;

    /// Group `triples` into leaves keyed by predicate, in `ord`'s axis.
    fn leaf_columns(triples: &[Triple], ord: Ordering) -> Vec<(TermId, Vec<TermId>, Vec<TermId>)> {
        let subject_major = matches!(ord, Ordering::Spo | Ordering::Sop | Ordering::Pso);
        let mut preds: Vec<TermId> = triples.iter().map(|t| t.p).collect();
        preds.sort_unstable();
        preds.dedup();
        preds
            .into_iter()
            .map(|p| {
                let mut rows: Vec<(TermId, TermId)> = triples
                    .iter()
                    .filter(|t| t.p == p)
                    .map(|t| {
                        if subject_major {
                            (t.s, t.o)
                        } else {
                            (t.o, t.s)
                        }
                    })
                    .collect();
                rows.sort_unstable();
                rows.dedup();
                (
                    p,
                    rows.iter().map(|r| r.0).collect(),
                    rows.iter().map(|r| r.1).collect(),
                )
            })
            .collect()
    }

    fn iter_of<'a>(
        cols: &'a [(TermId, Vec<TermId>, Vec<TermId>)],
        ord: Ordering,
    ) -> MergedIter<'a> {
        MergedIter::new(
            ord,
            cols.iter()
                .map(|(k, a, b)| MergedLeaf {
                    key: *k,
                    level0: a,
                    level1: b,
                })
                .collect(),
        )
    }

    fn sample_triples() -> Vec<Triple> {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut out = Vec::new();
        for s in 0..12u64 {
            for p in 100..104u64 {
                for _ in 0..(rand() % 4) {
                    out.push(Triple::new(s, p, rand() % 9));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Walk a cursor depth-first and collect every `(level0, level1, level2)`
    /// path it yields. Uses only the `OrderedTripleIter` contract, so the same
    /// walk drives both implementations.
    fn walk<I: OrderedTripleIter>(it: &mut I) -> Vec<(TermId, TermId, TermId)> {
        let mut out = Vec::new();
        while let Some(a) = it.peek(0) {
            it.open_level(1);
            while let Some(b) = it.peek(1) {
                it.open_level(2);
                while let Some(c) = it.peek(2) {
                    out.push((a, b, c));
                    it.seek(2, c.wrapping_add(1));
                }
                it.up(2);
                it.seek(1, b.wrapping_add(1));
            }
            it.up(1);
            it.seek(0, a.wrapping_add(1));
        }
        out
    }

    #[test]
    fn full_walk_matches_vec_source_in_every_ordering() {
        let triples = sample_triples();
        let vec_src = VecTripleSource::from_triples(triples.clone());
        for ord in Ordering::ALL {
            let cols = leaf_columns(&triples, ord);
            let expected = walk(&mut vec_src.iter(ord).unwrap());
            let got = walk(&mut iter_of(&cols, ord));
            assert_eq!(got, expected, "{ord:?}");
            assert_eq!(got.len(), triples.len(), "{ord:?} row count");
        }
    }

    #[test]
    fn seek_and_rewind_track_vec_source() {
        // Drive both cursors through the same pseudo-random script of
        // seek/open/up/rewind calls and compare every peek along the way.
        let triples = sample_triples();
        let vec_src = VecTripleSource::from_triples(triples.clone());
        let mut state = 12345u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for ord in Ordering::ALL {
            let cols = leaf_columns(&triples, ord);
            let mut a = vec_src.iter(ord).unwrap();
            let mut b = iter_of(&cols, ord);
            let mut depth = 0u8;
            for _ in 0..2000 {
                assert_eq!(b.peek(depth), a.peek(depth), "{ord:?} peek at {depth}");
                match rand() % 4 {
                    0 => {
                        let v = rand() % 14;
                        a.seek(depth, v);
                        b.seek(depth, v);
                    }
                    1 if depth < 2 && a.peek(depth).is_some() => {
                        depth += 1;
                        a.open_level(depth);
                        b.open_level(depth);
                    }
                    2 if depth > 0 => {
                        a.up(depth);
                        b.up(depth);
                        depth -= 1;
                    }
                    _ => {
                        a.rewind(depth);
                        b.rewind(depth);
                    }
                }
            }
        }
    }

    #[test]
    fn empty_leaves_are_dropped_and_peek_is_none() {
        let empty: Vec<(TermId, Vec<TermId>, Vec<TermId>)> = vec![(7, Vec::new(), Vec::new())];
        let mut it = iter_of(&empty, Ordering::Pso);
        assert_eq!(it.peek(0), None);
        // `PatternTrieIter` seeks a missed bound level to MAX and keeps going.
        it.seek(0, TermId::MAX);
        assert_eq!(it.peek(0), None);
    }
}
