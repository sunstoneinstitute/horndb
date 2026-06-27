//! `VecTripleSource` — sorted-`Vec` test double for `TripleSource`.
//!
//! All six orderings are materialised eagerly; suitable for tests and small
//! benches up to a few million triples.

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::source::soa::LevelColumn;
use crate::source::{OrderedTripleIter, TripleSource};

pub struct VecTripleSource {
    /// One sorted `Vec<(l0, l1, l2)>` per ordering.
    sorted: HashMap<Ordering, Vec<(TermId, TermId, TermId)>>,
    total: usize,
}

impl VecTripleSource {
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        let total = triples.len();
        let mut sorted = HashMap::with_capacity(6);
        for &ord in &Ordering::ALL {
            let mut v: Vec<_> = triples.iter().map(|t| t.by_ordering(ord)).collect();
            v.sort_unstable();
            v.dedup();
            sorted.insert(ord, v);
        }
        Self { sorted, total }
    }

    /// O(log n) membership test against the SPO-sorted ordering.
    pub fn contains(&self, t: &Triple) -> bool {
        let spo = &self.sorted[&Ordering::Spo];
        spo.binary_search(&(t.s, t.p, t.o)).is_ok()
    }
}

impl TripleSource for VecTripleSource {
    type Iter<'a> = VecIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<VecIter<'_>> {
        let data = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(VecIter::new(data))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Minimum active-run length for which we materialise a contiguous SoA
/// `LevelColumn` and seek through the SIMD `lower_bound`. Below this, the
/// strided AoS `partition_point` (itself auto-vectorised) is cheaper than the
/// column copy — and crucially, deep trie levels with tiny runs (e.g. an
/// out-degree of a handful) are opened-and-torn-down millions of times, so
/// building a column for each would dominate. Tuned so only the wide levels
/// (where SIMD `lower_bound` pays off) build a column.
const SIMD_SEEK_MIN_RUN: usize = 64;

/// Cursor state: at each depth we hold a `(lo, hi)` range into `data` of rows
/// whose prefix matches the chosen path so far. `cursor[depth]` is the index
/// of the next row to return at `depth`.
pub struct VecIter<'a> {
    data: &'a [(TermId, TermId, TermId)],
    /// (lo, hi) per depth — `hi` is exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
    /// SoA column for the active range at each depth, rebuilt on
    /// `open_level`/`up`. Built only for runs of at least [`SIMD_SEEK_MIN_RUN`]
    /// rows (where the SIMD `lower_bound` outweighs the column copy); shorter
    /// runs leave this `None` and `seek` falls back to the scalar AoS search.
    col_view: [Option<LevelColumn>; 3],
}

impl<'a> VecIter<'a> {
    pub(crate) fn new(data: &'a [(TermId, TermId, TermId)]) -> Self {
        let full = (0usize, data.len());
        Self {
            data,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
            // Columns are built lazily on the first seek of a wide-enough
            // level, so iters whose leading physical level is a bound term
            // (seeked once) never pay for a full-data column copy.
            col_view: [None, None, None],
        }
    }

    fn col(&self, row: usize, depth: u8) -> TermId {
        let t = self.data[row];
        match depth {
            0 => t.0,
            1 => t.1,
            2 => t.2,
            _ => unreachable!("depth {depth} > 2"),
        }
    }

    /// Scalar AoS lower-bound fallback for short runs (no SoA column). Finds the
    /// first row in `data[start..hi]` whose `depth` column is `>= value`.
    #[inline]
    fn seek_scalar(&self, depth: u8, start: usize, hi: usize, value: TermId) -> usize {
        let slice = &self.data[start..hi];
        let off = slice.partition_point(|row| {
            let v = match depth {
                0 => row.0,
                1 => row.1,
                2 => row.2,
                _ => unreachable!(),
            };
            v < value
        });
        start + off
    }
}

impl<'a> OrderedTripleIter for VecIter<'a> {
    #[inline]
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.col(c, depth))
    }

    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        // Build the SoA column lazily the first time a wide level is seeked;
        // short runs stay `None` and use the scalar AoS partition_point.
        if self.col_view[d].is_none() && hi - lo >= SIMD_SEEK_MIN_RUN {
            self.col_view[d] = Some(LevelColumn::from_aos(self.data, lo, hi, depth));
        }
        // Wide levels seek through the SIMD `lower_bound`; short runs fall back.
        self.cursor[d] = match self.col_view[d].as_ref() {
            Some(col) => col.lower_bound_from(start, value),
            None => self.seek_scalar(depth, start, hi, value),
        };
    }

    #[inline]
    fn open_level(&mut self, depth: u8) {
        assert!((1..=2).contains(&depth), "open_level depth must be 1 or 2");
        let parent = (depth - 1) as usize;
        let (_, hi_parent) = self.range[parent];
        let row = self.cursor[parent];
        let v = self.col(row, depth - 1);
        // Find the half-open range of rows in `[row, hi_parent)` whose
        // depth-(depth-1) column equals `v` AND prefix up to depth-2 matches.
        // Since rows are sorted and the prefix is already constrained, the
        // run with column == v is contiguous.
        let slice = &self.data[row..hi_parent];
        let end_off = slice.partition_point(|r| {
            let c = match depth - 1 {
                0 => r.0,
                1 => r.1,
                2 => r.2,
                _ => unreachable!(),
            };
            c <= v
        });
        let new_lo = row;
        let new_hi = row + end_off;
        self.range[depth as usize] = (new_lo, new_hi);
        self.cursor[depth as usize] = new_lo;
        // Invalidate any stale column from a previous sibling subtree at this
        // depth; a fresh one is built lazily on the first seek if wide enough.
        self.col_view[depth as usize] = None;
    }

    #[inline]
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            // Root: reset to full data range and rewind cursor to start. The
            // depth-0 column covers all rows and never changes — keep it.
            self.range[0] = (0, self.data.len());
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
            self.col_view[d] = None;
        }
    }

    #[inline]
    fn rewind(&mut self, depth: u8) {
        let d = depth as usize;
        self.cursor[d] = self.range[d].0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_finds_present_and_rejects_absent() {
        let src = VecTripleSource::from_triples(vec![
            Triple::new(1, 2, 3),
            Triple::new(1, 2, 4),
            Triple::new(5, 6, 7),
        ]);
        assert!(src.contains(&Triple::new(1, 2, 4)));
        assert!(!src.contains(&Triple::new(1, 2, 5)));
        assert!(!src.contains(&Triple::new(9, 9, 9)));
    }
}
