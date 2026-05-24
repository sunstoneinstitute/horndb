//! `VecTripleSource` — sorted-`Vec` test double for `TripleSource`.
//!
//! All six orderings are materialised eagerly; suitable for tests and small
//! benches up to a few million triples.

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
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
}

impl TripleSource for VecTripleSource {
    fn iter(&self, ord: Ordering) -> Result<Box<dyn OrderedTripleIter + '_>> {
        let data = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(Box::new(VecIter::new(data)))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Cursor state: at each depth we hold a `(lo, hi)` range into `data` of rows
/// whose prefix matches the chosen path so far. `cursor[depth]` is the index
/// of the next row to return at `depth`.
struct VecIter<'a> {
    data: &'a [(TermId, TermId, TermId)],
    /// (lo, hi) per depth — `hi` is exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
}

impl<'a> VecIter<'a> {
    fn new(data: &'a [(TermId, TermId, TermId)]) -> Self {
        let full = (0usize, data.len());
        Self {
            data,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
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
}

impl<'a> OrderedTripleIter for VecIter<'a> {
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.col(c, depth))
    }

    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        // Binary search the suffix `data[start..hi]` for the first row whose
        // `depth` column is ≥ `value`. Because rows share a common prefix at
        // depths < `depth`, the `depth` column is monotone non-decreasing
        // within `[lo, hi)`.
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
        self.cursor[d] = start + off;
    }

    fn open_level(&mut self, depth: u8) {
        assert!(depth >= 1 && depth <= 2, "open_level depth must be 1 or 2");
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
    }

    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            // Root: reset to full data range and rewind cursor to start.
            self.range[0] = (0, self.data.len());
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
        }
    }
}
