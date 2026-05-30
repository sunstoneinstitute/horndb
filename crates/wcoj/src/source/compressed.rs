//! `CompressedTripleSource` — a memory-compact `TripleSource`.
//!
//! Same external behaviour as [`crate::source::vec_source::VecTripleSource`]
//! (all six orderings materialised, sorted, deduped), but each ordering's
//! three columns are stored as [`PackedColumn`]s (frame-of-reference +
//! bit-packing) instead of a dense `Vec<(u64,u64,u64)>`. The trie cursor
//! semantics are identical to `VecIter`; only the physical reads differ.

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::source::packed_column::PackedColumn;
use crate::source::{OrderedTripleIter, TripleSource};

/// Three packed columns (level 0, 1, 2) for one ordering, plus row count.
struct OrderColumns {
    cols: [PackedColumn; 3],
    rows: usize,
}

pub struct CompressedTripleSource {
    sorted: HashMap<Ordering, OrderColumns>,
    total: usize,
}

impl CompressedTripleSource {
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        let total = triples.len();
        let mut sorted = HashMap::with_capacity(6);
        for &ord in &Ordering::ALL {
            let mut rows: Vec<(TermId, TermId, TermId)> =
                triples.iter().map(|t| t.by_ordering(ord)).collect();
            rows.sort_unstable();
            rows.dedup();
            let l0: Vec<u64> = rows.iter().map(|r| r.0).collect();
            let l1: Vec<u64> = rows.iter().map(|r| r.1).collect();
            let l2: Vec<u64> = rows.iter().map(|r| r.2).collect();
            sorted.insert(
                ord,
                OrderColumns {
                    cols: [
                        PackedColumn::from_slice(&l0),
                        PackedColumn::from_slice(&l1),
                        PackedColumn::from_slice(&l2),
                    ],
                    rows: rows.len(),
                },
            );
        }
        Self { sorted, total }
    }

    /// Compressed payload bytes across every materialised ordering (the packed
    /// word streams + per-block metadata; excludes `Vec`/`HashMap` bookkeeping).
    /// Used by the bench to report bytes/triple against the dense
    /// `VecTripleSource`.
    pub fn heap_bytes(&self) -> usize {
        self.sorted
            .values()
            .map(|o| o.cols.iter().map(|c| c.heap_bytes()).sum::<usize>())
            .sum()
    }
}

impl TripleSource for CompressedTripleSource {
    type Iter<'a> = CompressedIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<CompressedIter<'_>> {
        let oc = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(CompressedIter::new(&oc.cols, oc.rows))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Cursor over three [`PackedColumn`]s. Field-for-field analogue of
/// [`crate::source::vec_source::VecIter`].
pub struct CompressedIter<'a> {
    cols: &'a [PackedColumn; 3],
    rows: usize,
    /// (lo, hi) per depth — `hi` exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
}

impl<'a> CompressedIter<'a> {
    pub(crate) fn new(cols: &'a [PackedColumn; 3], rows: usize) -> Self {
        Self {
            cols,
            rows,
            range: [(0, rows), (0, 0), (0, 0)],
            cursor: [0, 0, 0],
        }
    }
}

impl<'a> OrderedTripleIter for CompressedIter<'a> {
    #[inline]
    fn peek(&self, depth: u8) -> Option<TermId> {
        let (lo, hi) = self.range[depth as usize];
        let c = self.cursor[depth as usize].max(lo);
        if c >= hi {
            return None;
        }
        Some(self.cols[depth as usize].get(c))
    }

    #[inline]
    fn seek(&mut self, depth: u8, value: TermId) {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        self.cursor[d] = self.cols[d].lower_bound(start, hi, value);
    }

    #[inline]
    fn open_level(&mut self, depth: u8) {
        assert!((1..=2).contains(&depth), "open_level depth must be 1 or 2");
        let parent = (depth - 1) as usize;
        let (_, hi_parent) = self.range[parent];
        let row = self.cursor[parent];
        debug_assert!(
            row < self.rows,
            "open_level called with exhausted parent cursor"
        );
        let v = self.cols[parent].get(row);
        // Contiguous run in [row, hi_parent) whose parent column == v.
        let new_hi = self.cols[parent].upper_bound(row, hi_parent, v);
        self.range[depth as usize] = (row, new_hi);
        self.cursor[depth as usize] = row;
    }

    #[inline]
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            self.range[0] = (0, self.rows);
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
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
    use crate::source::vec_source::VecTripleSource;

    fn sample_triples() -> Vec<Triple> {
        vec![
            Triple::new(1, 10, 2),
            Triple::new(1, 10, 5),
            Triple::new(2, 10, 3),
            Triple::new(2, 11, 3),
            Triple::new(4, 10, 1),
        ]
    }

    /// Walking one ordering with a manual peek/open_level/up sequence must
    /// yield the same values as the dense `VecIter`.
    #[test]
    fn matches_vec_iter_walk_spo() {
        let triples = sample_triples();
        let comp = CompressedTripleSource::from_triples(triples.clone());
        let dense = VecTripleSource::from_triples(triples);

        let mut ci = comp.iter(Ordering::Spo).unwrap();
        let mut vi = dense.iter(Ordering::Spo).unwrap();

        // Depth 0: iterate every distinct subject, descending into each.
        loop {
            let (cs, vs) = (ci.peek(0), vi.peek(0));
            assert_eq!(cs, vs);
            if cs.is_none() {
                break;
            }
            ci.open_level(1);
            vi.open_level(1);
            loop {
                assert_eq!(ci.peek(1), vi.peek(1));
                if ci.peek(1).is_none() {
                    break;
                }
                ci.open_level(2);
                vi.open_level(2);
                loop {
                    assert_eq!(ci.peek(2), vi.peek(2));
                    if ci.peek(2).is_none() {
                        break;
                    }
                    ci.seek(2, ci.peek(2).unwrap() + 1);
                    vi.seek(2, vi.peek(2).unwrap() + 1);
                }
                ci.up(2);
                vi.up(2);
                ci.seek(1, ci.peek(1).unwrap() + 1);
                vi.seek(1, vi.peek(1).unwrap() + 1);
            }
            ci.up(1);
            vi.up(1);
            ci.seek(0, ci.peek(0).unwrap() + 1);
            vi.seek(0, vi.peek(0).unwrap() + 1);
        }
    }

    #[test]
    fn total_triples_matches() {
        let triples = sample_triples();
        let comp = CompressedTripleSource::from_triples(triples.clone());
        let dense = VecTripleSource::from_triples(triples);
        assert_eq!(comp.total_triples(), dense.total_triples());
    }

    #[test]
    fn heap_bytes_is_smaller_for_constant_predicate() {
        // Single-predicate graph: l0 of Pso/Pos is constant → near-zero bits.
        let triples: Vec<Triple> = (0..1000u64)
            .map(|s| Triple::new(s, 10, (s * 7) % 1000))
            .collect();
        let comp = CompressedTripleSource::from_triples(triples);
        // 1000 triples × 6 orderings × 24 bytes dense = 144_000 bytes of payload.
        // Compressed must be well under half that.
        assert!(
            comp.heap_bytes() < 72_000,
            "compressed heap_bytes={} not < 72000",
            comp.heap_bytes()
        );
    }
}
