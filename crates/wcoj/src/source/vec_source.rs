//! `VecTripleSource` — sorted, column-major test double for `TripleSource`.
//!
//! All six orderings are materialised eagerly; suitable for tests and small
//! benches up to a few million triples.
//!
//! Each ordering is stored **column-major** (struct-of-arrays): three
//! `Vec<TermId>`, one per trie level, instead of one `Vec<(TermId, TermId,
//! TermId)>` of rows. A trie level's values are then already contiguous, so the
//! SIMD primitives (`horndb_simd::lower_bound`, `horndb_simd::intersect`) read
//! them directly — no per-level copy out of a strided row layout (SPEC-03 NF2).

use std::collections::HashMap;

use crate::error::{Result, WcojError};
use crate::ids::{Ordering, TermId, Triple};
use crate::source::{OrderedTripleIter, TripleSource};

/// One ordering's sorted rows, column-major. `levels[d][row]` is that row's
/// value at trie depth `d`; all three columns have the same length.
struct OrderedColumns {
    levels: [Vec<TermId>; 3],
}

impl OrderedColumns {
    fn view(&self) -> SortedColumns<'_> {
        SortedColumns {
            levels: [&self.levels[0], &self.levels[1], &self.levels[2]],
        }
    }
}

/// Borrowed column-major view of one ordering's sorted rows.
///
/// The columns are in `ord`'s axis order — the `Triple::by_ordering` layout
/// `from_triples` sorts by. So levels 0/1/2 are `ord`'s `(level0, level1,
/// level2)`: for `Pso` that is `(predicate, subject, object)`; for `Pos` it is
/// `(predicate, object, subject)`.
#[derive(Clone, Copy)]
pub struct SortedColumns<'a> {
    levels: [&'a [TermId]; 3],
}

impl<'a> SortedColumns<'a> {
    /// The whole column at trie depth `level` (0, 1 or 2).
    pub fn level(&self, level: usize) -> &'a [TermId] {
        self.levels[level]
    }

    /// Number of rows (identical for all three columns).
    pub fn len(&self) -> usize {
        self.levels[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels[0].is_empty()
    }

    /// Row `i` reassembled as a tuple, in `ord`'s axis order.
    pub fn row(&self, i: usize) -> (TermId, TermId, TermId) {
        (self.levels[0][i], self.levels[1][i], self.levels[2][i])
    }
}

pub struct VecTripleSource {
    /// One sorted, column-major index per ordering.
    sorted: HashMap<Ordering, OrderedColumns>,
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
            // Split the deduplicated rows into three contiguous columns.
            let mut levels = [
                Vec::with_capacity(v.len()),
                Vec::with_capacity(v.len()),
                Vec::with_capacity(v.len()),
            ];
            for (l0, l1, l2) in v {
                levels[0].push(l0);
                levels[1].push(l1);
                levels[2].push(l2);
            }
            sorted.insert(ord, OrderedColumns { levels });
        }
        Self { sorted, total }
    }

    /// O(log n) membership test against the SPO-sorted ordering: narrow to the
    /// subject's row range, then to the predicate's, then look for the object.
    pub fn contains(&self, t: &Triple) -> bool {
        let cols = &self.sorted[&Ordering::Spo];
        let (s_col, p_col, o_col) = (&cols.levels[0], &cols.levels[1], &cols.levels[2]);
        let s_lo = s_col.partition_point(|&v| v < t.s);
        let s_hi = s_lo + s_col[s_lo..].partition_point(|&v| v <= t.s);
        let p_lo = s_lo + p_col[s_lo..s_hi].partition_point(|&v| v < t.p);
        let p_hi = p_lo + p_col[p_lo..s_hi].partition_point(|&v| v <= t.p);
        let idx = p_lo + o_col[p_lo..p_hi].partition_point(|&v| v < t.o);
        idx < p_hi && o_col[idx] == t.o
    }

    /// The snapshot's triples sorted in `ord`, or `None` if that ordering is
    /// unavailable. Read-only view used by `SnapshotStats` to compute statistics
    /// by a single linear scan. See [`SortedColumns`] for the axis order.
    pub fn sorted_rows(&self, ord: Ordering) -> Option<SortedColumns<'_>> {
        self.sorted.get(&ord).map(OrderedColumns::view)
    }
}

impl TripleSource for VecTripleSource {
    type Iter<'a> = VecIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<VecIter<'_>> {
        let cols = self
            .sorted
            .get(&ord)
            .ok_or(WcojError::OrderingUnavailable(ord))?;
        Ok(VecIter::new(cols.view()))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// Minimum active-run length for which `active_run` exposes a level to the
/// leapfrog's SIMD `intersect` fast path. Below this, the scalar round-robin
/// seek is cheaper than arming the intersect.
const SIMD_SEEK_MIN_RUN: usize = 64;

/// Cursor state: at each depth we hold a `(lo, hi)` row range whose prefix
/// matches the chosen path so far. `cursor[depth]` is the index of the next row
/// to return at `depth`.
pub struct VecIter<'a> {
    /// One contiguous column per trie depth, all of the same length.
    cols: [&'a [TermId]; 3],
    /// (lo, hi) per depth — `hi` is exclusive.
    range: [(usize, usize); 3],
    /// Cursor index per depth.
    cursor: [usize; 3],
    /// Cached distinct-key copy of the active range at depths 0 and 1, built on
    /// demand by `active_run` (see there for why the leaf needs none).
    distinct: [Option<Vec<TermId>>; 3],
}

impl<'a> VecIter<'a> {
    pub(crate) fn new(cols: SortedColumns<'a>) -> Self {
        let full = (0usize, cols.len());
        Self {
            cols: cols.levels,
            range: [full, (0, 0), (0, 0)],
            cursor: [0, 0, 0],
            distinct: [None, None, None],
        }
    }

    #[inline]
    fn col(&self, row: usize, depth: u8) -> TermId {
        self.cols[depth as usize][row]
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
        let col = self.cols[col_depth as usize];
        let n = hi - row;
        // Gallop a window `[row + lo_off, row + hi_off)` that brackets the
        // boundary: `col(row + lo_off) <= v` stays true, `col(row + hi_off) > v`
        // (or `hi_off == n`).
        let mut lo_off = 0usize;
        let mut step = 1usize;
        while lo_off + step < n && col[row + lo_off + step] <= v {
            lo_off += step;
            step <<= 1;
        }
        let hi_off = (lo_off + step).min(n);
        // Binary-search the bracketed window for the first `col > v`.
        let off = col[row + lo_off..row + hi_off].partition_point(|&c| c <= v);
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
        let col = self.cols[depth as usize];
        let n = hi - start;
        if n == 0 {
            return Some(hi);
        }
        if col[start] >= value {
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
            if col[start + probe] >= value {
                break probe; // boundary in `(lo, probe]`
            }
            lo = probe;
            step <<= 1;
        };
        let off = col[start + lo..start + hi_off].partition_point(|&c| c < value);
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
        // Resolves that case without touching a wide binary search.
        if let Some(idx) = self.seek_gallop(depth, start, hi, value) {
            self.cursor[d] = idx;
            return;
        }
        // Gallop miss: by construction the target is more than `GALLOP_CAP`
        // (64) rows away, so pay the exact lower bound over the rest of the
        // level. Columns are stored contiguously, so this is a direct SIMD
        // `lower_bound` at *every* depth — the old rule that only the depth-0
        // full-data level was worth the SIMD path (and the ~760× `four_cycle`
        // regression from rebuilding a per-`open_level` column) was a property
        // of the row-major layout, which is gone.
        self.cursor[d] = start + horndb_simd::lower_bound(&self.cols[d][start..hi], value);
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
        // Drop any distinct-key cache from a previous sibling subtree at this
        // depth; a fresh one is built on demand by `active_run`.
        self.distinct[depth as usize] = None;
    }

    #[inline]
    fn up(&mut self, depth: u8) {
        let d = depth as usize;
        if d == 0 {
            // Root: reset to full data range and rewind cursor to start. The
            // depth-0 distinct cache covers all rows and never changes — keep it.
            self.range[0] = (0, self.cols[0].len());
            self.cursor[0] = 0;
        } else {
            self.range[d] = (0, 0);
            self.cursor[d] = 0;
            self.distinct[d] = None;
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
        // Short runs stay scalar and opt out of the SIMD intersect fast path.
        if hi - lo < SIMD_SEEK_MIN_RUN {
            return None;
        }
        if d == 2 {
            // Leaf: `open_level(2)` fixed the parent prefix (level0, level1) and
            // the rows are deduplicated triples, so the leaf column over this
            // range is already strictly increasing — exactly the distinct-key
            // contract the leapfrog and `horndb_simd::intersect` require. Hand
            // out the stored column with no copy and no dedup.
            return Some(&self.cols[2][start..hi]);
        }
        // Inner levels: the column repeats a key once per child row (a subject
        // with several objects), but the leapfrog and `intersect` operate on
        // distinct level keys — so dedup into a cached buffer.
        let cursor_val = self.cols[d][start];
        if self.distinct[d].is_none() {
            let src = &self.cols[d][lo..hi];
            let mut out = Vec::with_capacity(src.len());
            for &v in src {
                if out.last() != Some(&v) {
                    out.push(v);
                }
            }
            self.distinct[d] = Some(out);
        }
        let distinct = self.distinct[d].as_ref()?;
        // Start at the first distinct key >= the key under the cursor (the
        // cursor may have advanced past the level start before arming).
        let off = distinct.partition_point(|&k| k < cursor_val);
        Some(&distinct[off..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split test rows written as tuples into the three stored columns.
    fn columns_of(rows: &[(TermId, TermId, TermId)]) -> ([Vec<TermId>; 3], usize) {
        let mut levels = [Vec::new(), Vec::new(), Vec::new()];
        for &(a, b, c) in rows {
            levels[0].push(a);
            levels[1].push(b);
            levels[2].push(c);
        }
        let n = rows.len();
        (levels, n)
    }

    fn view(levels: &[Vec<TermId>; 3]) -> SortedColumns<'_> {
        SortedColumns {
            levels: [&levels[0], &levels[1], &levels[2]],
        }
    }

    #[test]
    fn run_end_matches_partition_point() {
        // Column 0 with runs of varying length inside a wider range; the gallop
        // must land on the same boundary as a straight `partition_point`.
        let rows: Vec<(TermId, TermId, TermId)> = vec![
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
        let (levels, n) = columns_of(&rows);
        let it = VecIter::new(view(&levels));
        let c0 = &levels[0];
        // For every start row, run_end from that row must equal the scalar
        // partition_point end of the run of `c0[row]`.
        for row in 0..n {
            let v = c0[row];
            let expect = row + c0[row..n].partition_point(|&c| c <= v);
            assert_eq!(it.run_end(0, row, n, v), expect, "row {row}, v {v}");
            // A narrower `hi` must clamp the answer.
            for hi in row..=n {
                let expect_hi = row + c0[row..hi].partition_point(|&c| c <= v);
                assert_eq!(it.run_end(0, row, hi, v), expect_hi, "row {row}, hi {hi}");
            }
        }
    }

    #[test]
    fn seek_matches_lower_bound_oracle_near_and_far() {
        // Depth-0 column spanning > GALLOP_CAP (64) rows so both the gallop-hit
        // (near target) and gallop-miss (far target → binary-search fallback)
        // paths are exercised. Column 0 = row/3 gives runs of length 3.
        let rows: Vec<(TermId, TermId, TermId)> = (0..300u64).map(|i| (i / 3, i % 3, 0)).collect();
        let (levels, n) = columns_of(&rows);
        let c0 = &levels[0];
        let max_key = (n as u64 - 1) / 3;
        // For every starting cursor and every target, the post-seek cursor must
        // equal the scalar lower bound over `[start, n)`.
        for &start in &[0usize, 1, 5, 50, 100, 250, n - 1] {
            for value in 0..=(max_key + 2) {
                let mut it = VecIter::new(view(&levels));
                it.cursor[0] = start;
                it.seek(0, value);
                let oracle = start + c0[start..n].partition_point(|&c| c < value);
                assert_eq!(it.cursor[0], oracle, "start {start}, value {value}");
            }
        }
    }

    #[test]
    fn active_run_dedups_inner_level_and_skips_to_cursor() {
        // Depth-0 column where each key carries three child rows. The stored
        // column keeps the duplicates; `active_run` must expose distinct keys
        // from the cursor onward. 32 keys × 3 rows clears SIMD_SEEK_MIN_RUN.
        let rows: Vec<(TermId, TermId, TermId)> = (0..96u64).map(|i| (i / 3, i % 3, 0)).collect();
        let (levels, _) = columns_of(&rows);
        let expect: Vec<TermId> = (0..32u64).collect();

        let mut it = VecIter::new(view(&levels));
        assert_eq!(it.active_run(0).unwrap(), &expect[..]);

        // Cursor parked inside the run of key 7 → keys >= 7.
        let mut it = VecIter::new(view(&levels));
        it.cursor[0] = 7 * 3 + 1;
        assert_eq!(it.active_run(0).unwrap(), &expect[7..]);

        // Short runs opt out of the fast path entirely.
        let short: Vec<(TermId, TermId, TermId)> = (0..10u64).map(|i| (i, 0, 0)).collect();
        let (short_levels, _) = columns_of(&short);
        let mut it = VecIter::new(view(&short_levels));
        assert!(it.active_run(0).is_none());
    }

    #[test]
    fn active_run_leaf_is_strictly_increasing_from_cursor() {
        // One subject/predicate prefix with 100 distinct objects: after
        // `open_level(2)` the leaf column is the objects, already distinct.
        let triples: Vec<Triple> = (0..100u64).map(|o| Triple::new(1, 2, o)).collect();
        let src = VecTripleSource::from_triples(triples);
        let mut it = src.iter(Ordering::Spo).expect("Spo ordering");
        it.open_level(1);
        it.open_level(2);
        let run = it.active_run(2).expect("leaf run >= SIMD_SEEK_MIN_RUN");
        assert_eq!(run.len(), 100);
        assert_eq!(run[0], 0);
        assert!(run.windows(2).all(|w| w[0] < w[1]), "strictly increasing");

        // From an advanced cursor the run starts at the cursor's value.
        it.seek(2, 40);
        let run = it.active_run(2).expect("leaf run still armed");
        assert_eq!(run[0], 40);
        assert_eq!(run.len(), 60);
        assert!(run.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
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
        assert!(!src.contains(&Triple::new(1, 3, 4)));
        assert!(!src.contains(&Triple::new(9, 9, 9)));
    }
}
