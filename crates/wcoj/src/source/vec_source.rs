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

    /// The snapshot's triples sorted in `ord`, or `None` if that ordering is
    /// unavailable. Read-only view used by `SnapshotStats` to compute statistics
    /// by a single linear scan.
    ///
    /// Each tuple is stored in `ord`'s axis order — the same `Triple::by_ordering`
    /// layout `from_triples` fills `sorted` with. So `.0/.1/.2` are `ord`'s
    /// `(level0, level1, level2)`: for `Pso` that is `(predicate, subject, object)`;
    /// for `Pos` it is `(predicate, object, subject)`.
    pub fn sorted_rows(&self, ord: Ordering) -> Option<&[(TermId, TermId, TermId)]> {
        self.sorted.get(&ord).map(Vec::as_slice)
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
/// column copy.
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
    /// SoA column for the active range at each depth. The hot `seek` path only
    /// auto-builds **depth 0** — the full-data root level, which is built once
    /// (lazily) and reused for the whole scan (it survives `up(0)`). Depths 1–2
    /// have per-`open_level` ranges that, when wide (e.g. under a single bound
    /// predicate), would otherwise be rebuilt on every descent — an O(range)
    /// cost in the inner loop that dwarfs the scalar AoS `partition_point` — so
    /// they stay scalar on the hot path. `active_run` (the leapfrog SIMD
    /// intersect, off the executor's inlined hot path) may still build a deeper
    /// column on demand.
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

    /// First absolute index in `[row, hi)` whose `col_depth` column is `> v`,
    /// found by a bounded gallop from `row`. This is the end of the contiguous
    /// run of rows equal to `v` that starts at `row` (rows are sorted and the
    /// parent prefix is fixed, so that run is contiguous).
    ///
    /// Why gallop instead of `partition_point`: on a descent (`open_level`) the
    /// child run is typically short but the parent range is wide — a subject
    /// with 8 objects sitting inside a 400k-row predicate block. A binary
    /// search bisects the *whole* wide range (~log(range) probes scattered
    /// across memory, each a cache miss); an exponential probe from the cursor
    /// reaches the boundary in ~log(run) cache-local steps. It is never
    /// asymptotically worse than binary search (the final window is
    /// binary-searched), so wide runs (e.g. hub subjects) are unaffected.
    #[inline]
    fn run_end(&self, col_depth: u8, row: usize, hi: usize, v: TermId) -> usize {
        let n = hi - row;
        // Gallop a window `[row + lo_off, row + hi_off)` that brackets the
        // boundary: `col(row + lo_off) <= v` stays true, `col(row + hi_off) > v`
        // (or `hi_off == n`).
        let mut lo_off = 0usize;
        let mut step = 1usize;
        while lo_off + step < n && self.col(row + lo_off + step, col_depth) <= v {
            lo_off += step;
            step <<= 1;
        }
        let hi_off = (lo_off + step).min(n);
        // Binary-search the bracketed window for the first `col > v`.
        let slice = &self.data[row + lo_off..row + hi_off];
        let off = slice.partition_point(|r| {
            let c = match col_depth {
                0 => r.0,
                1 => r.1,
                2 => r.2,
                _ => unreachable!(),
            };
            c <= v
        });
        row + lo_off + off
    }

    /// Cache-local fast path for the common leapfrog seek: the cursor advances
    /// monotonically, so the target usually sits just past `start`. Probe a
    /// bounded window (≤ `GALLOP_CAP` rows) from the cursor and, if the lower
    /// bound lands inside it, return it exactly. Returns `None` when the target
    /// is farther than the window and data still remains — the caller then runs
    /// the full binary search, so a far ("SPB-style") seek keeps its exact
    /// behaviour and pays only ~log2(cap) extra cache-local probes first.
    ///
    /// The returned index (when `Some`) is identical to `lower_bound` — the
    /// first row in `[start, hi)` whose `depth` column is `>= value`.
    #[inline]
    fn seek_gallop(&self, depth: u8, start: usize, hi: usize, value: TermId) -> Option<usize> {
        const GALLOP_CAP: usize = 64;
        let n = hi - start;
        if n == 0 {
            return Some(hi);
        }
        if self.col(start, depth) >= value {
            // Cursor already at/past the target — the overwhelmingly common
            // leapfrog case (peek was already >= the seek target).
            return Some(start);
        }
        // `col(start) < value`. Gallop a window `(lo, hi_off]` bracketing the
        // boundary, capped so a far target bails to the binary search.
        let mut lo = 0usize;
        let mut step = 1usize;
        let hi_off = loop {
            let probe = lo + step;
            if probe >= n {
                break n; // boundary is within `(lo, n)`
            }
            if probe > GALLOP_CAP {
                return None; // far target, data remains → caller binary-searches
            }
            if self.col(start + probe, depth) >= value {
                break probe; // boundary in `(lo, probe]`
            }
            lo = probe;
            step <<= 1;
        };
        let slice = &self.data[start + lo..start + hi_off];
        let off = slice.partition_point(|r| {
            let c = match depth {
                0 => r.0,
                1 => r.1,
                2 => r.2,
                _ => unreachable!(),
            };
            c < value
        });
        Some(start + lo + off)
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
        // Cache-local bounded gallop from the cursor first: the leapfrog seeks
        // monotonically forward, so the target is usually within a few rows.
        // Resolves that case without touching a wide binary search (and without
        // building a SoA column). A far target returns `None` and falls through.
        if let Some(idx) = self.seek_gallop(depth, start, hi, value) {
            self.cursor[d] = idx;
            return;
        }
        // Build the depth-0 (root) SoA column lazily on first (far) seek; it
        // covers the full data, is built once, and is reused for the whole scan.
        // Deeper levels stay scalar here to avoid a per-`open_level` rebuild in
        // the inner loop (see `col_view` docs).
        if d == 0 && self.col_view[0].is_none() && hi - lo >= SIMD_SEEK_MIN_RUN {
            self.col_view[0] = Some(LevelColumn::from_aos(self.data, lo, hi, 0));
        }
        // Levels with a column seek through the SIMD `lower_bound`; the rest
        // fall back to the scalar AoS partition_point.
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
        // run with column == v is contiguous. `run_end` gallops from the
        // cursor rather than bisecting the whole (wide) parent range.
        let new_lo = row;
        let new_hi = self.run_end(depth - 1, row, hi_parent, v);
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

    fn active_run(&mut self, depth: u8) -> Option<&[TermId]> {
        let d = depth as usize;
        let (lo, hi) = self.range[d];
        let start = self.cursor[d].max(lo);
        if start >= hi {
            return None;
        }
        // Materialise the column on demand (same threshold as `seek`); short
        // runs stay scalar and opt out of the SIMD intersect fast path.
        if self.col_view[d].is_none() {
            if hi - lo < SIMD_SEEK_MIN_RUN {
                return None;
            }
            self.col_view[d] = Some(LevelColumn::from_aos(self.data, lo, hi, depth));
        }
        let col = self.col_view[d].as_mut()?;
        // The leapfrog needs the level's *distinct* keys from the cursor on:
        // the raw column repeats a key once per child row (e.g. a subject with
        // several objects), but the SIMD `intersect` and the leapfrog itself
        // operate on distinct level keys. `distinct_run` dedups (cached).
        Some(col.distinct_run(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_end_matches_partition_point() {
        // Column 0 with runs of varying length inside a wider range; the gallop
        // must land on the same boundary as a straight `partition_point`.
        let data: Vec<(TermId, TermId, TermId)> = vec![
            (0, 0, 0),
            (0, 1, 0),
            (0, 2, 0), // run of 0 = [0, 3)
            (1, 0, 0), // run of 1 = [3, 4)
            (5, 0, 0),
            (5, 1, 0),
            (5, 2, 0),
            (5, 3, 0), // run of 5 = [4, 8)
            (9, 0, 0), // run of 9 = [8, 9)
        ];
        let it = VecIter::new(&data);
        let n = data.len();
        // For every start row, run_end from that row must equal the scalar
        // partition_point end of the run of `data[row].0`.
        for row in 0..n {
            let v = data[row].0;
            let expect = row + data[row..n].partition_point(|r| r.0 <= v);
            assert_eq!(it.run_end(0, row, n, v), expect, "row {row}, v {v}");
            // A narrower `hi` must clamp the answer.
            for hi in row..=n {
                let expect_hi = row + data[row..hi].partition_point(|r| r.0 <= v);
                assert_eq!(it.run_end(0, row, hi, v), expect_hi, "row {row}, hi {hi}");
            }
        }
    }

    #[test]
    fn seek_matches_lower_bound_oracle_near_and_far() {
        // Depth-0 column spanning > GALLOP_CAP (64) rows so both the gallop-hit
        // (near target) and gallop-miss (far target → binary-search fallback)
        // paths are exercised. Column 0 = row/3 gives runs of length 3.
        let data: Vec<(TermId, TermId, TermId)> = (0..300u64).map(|i| (i / 3, i % 3, 0)).collect();
        let n = data.len();
        let max_key = (n as u64 - 1) / 3;
        // For every starting cursor and every target, the post-seek cursor must
        // equal the scalar lower bound over `[start, n)`.
        for &start in &[0usize, 1, 5, 50, 100, 250, n - 1] {
            for value in 0..=(max_key + 2) {
                let mut it = VecIter::new(&data);
                it.cursor[0] = start;
                it.seek(0, value);
                let oracle = start + data[start..n].partition_point(|r| r.0 < value);
                assert_eq!(it.cursor[0], oracle, "start {start}, value {value}");
            }
        }
    }

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
