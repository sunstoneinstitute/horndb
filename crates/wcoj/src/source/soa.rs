//! Transient column-major (SoA) view of one trie level's active range.
//!
//! The dense `VecTripleSource` stores rows as AoS `(u64, u64, u64)`. A single
//! column over a `[lo, hi)` range is therefore strided, which the SIMD
//! `lower_bound`/`intersect` primitives can't consume directly (they want a
//! contiguous `&[u64]`). `LevelColumn` extracts one column of `[lo, hi)` into a
//! contiguous buffer once, so repeated seeks within that level are SIMD-friendly.
//!
//! Rebuilt on each `open_level`; the cost (one strided copy of the active run)
//! is amortised over the seeks the leapfrog performs within the level.

use crate::ids::TermId;

/// A contiguous copy of one column over a trie level's `[lo, hi)` active range.
pub(crate) struct LevelColumn {
    /// Column values for rows `lo..hi`, contiguous. Kept 1:1 with the source
    /// rows so `lower_bound_from` can map an offset back to an absolute row
    /// index — it therefore retains duplicate keys (e.g. a subject with
    /// several objects repeats at the subject level).
    values: Vec<TermId>,
    /// The `lo` the column was built from, so callers map absolute row indices.
    base: usize,
    /// Lazily-built deduplicated copy of `values` for the leapfrog SIMD
    /// intersect fast path. `intersect` requires sorted, duplicate-free input
    /// (and a trie level's logical keys *are* distinct), but `values` must
    /// keep its duplicates for the seek index mapping — so the distinct view
    /// is a separate buffer. Built on first `distinct_run` call and dropped
    /// with the column when `open_level`/`up` invalidates `col_view`.
    distinct: Option<Vec<TermId>>,
}

impl LevelColumn {
    /// Extract `data[lo..hi]`'s `depth` column into a contiguous buffer.
    pub(crate) fn from_aos(
        data: &[(TermId, TermId, TermId)],
        lo: usize,
        hi: usize,
        depth: u8,
    ) -> Self {
        let mut values = Vec::with_capacity(hi - lo);
        for row in &data[lo..hi] {
            let v = match depth {
                0 => row.0,
                1 => row.1,
                2 => row.2,
                _ => unreachable!("depth {depth} > 2"),
            };
            values.push(v);
        }
        Self {
            values,
            base: lo,
            distinct: None,
        }
    }

    /// First absolute row index in `[start_abs, base+len)` whose value is
    /// `>= value`, using the SIMD lower_bound. Returns an absolute
    /// (data-relative) index.
    pub(crate) fn lower_bound_from(&self, start_abs: usize, value: TermId) -> usize {
        let start_rel = start_abs - self.base;
        let off = horndb_simd::lower_bound(&self.values[start_rel..], value);
        start_abs + off
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &[TermId] {
        &self.values
    }

    /// Deduplicated, sorted distinct keys of the active run *at or after*
    /// `start_abs` — the contract `active_run` exposes to the leapfrog SIMD
    /// intersect. `values` is sorted, so dedup is a single linear pass over
    /// the column, cached on first use. The returned slice begins at the
    /// first distinct key `>=` the key currently under the cursor.
    pub(crate) fn distinct_run(&mut self, start_abs: usize) -> &[TermId] {
        if self.distinct.is_none() {
            let mut d = Vec::with_capacity(self.values.len());
            for &v in &self.values {
                if d.last() != Some(&v) {
                    d.push(v);
                }
            }
            self.distinct = Some(d);
        }
        let distinct = self.distinct.as_ref().unwrap();
        let start_rel = start_abs - self.base;
        if start_rel == 0 {
            return distinct;
        }
        // Skip distinct keys below the cursor's current key (the cursor may
        // have advanced past the level start before the fast path arms).
        let cursor_val = self.values[start_rel];
        let off = distinct.partition_point(|&k| k < cursor_val);
        &distinct[off..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_bound_matches_scalar() {
        // Column 1 (middle) of an AoS run.
        let data = vec![(0, 2, 9), (0, 4, 8), (0, 4, 1), (0, 7, 3), (0, 9, 0)];
        let col = LevelColumn::from_aos(&data, 0, data.len(), 1);
        assert_eq!(col.values(), &[2, 4, 4, 7, 9]);
        assert_eq!(col.lower_bound_from(0, 4), 1);
        assert_eq!(col.lower_bound_from(0, 5), 3);
        assert_eq!(col.lower_bound_from(2, 4), 2); // start past first 4
        assert_eq!(col.lower_bound_from(0, 10), 5);
    }

    #[test]
    fn distinct_run_dedups_and_skips_to_cursor() {
        // Subject column of a level where each subject carries several rows;
        // `values` keeps the duplicates (for seek index mapping) but
        // `distinct_run` must expose the deduplicated keys.
        let data = vec![
            (4, 0, 0),
            (4, 0, 1),
            (4, 0, 2),
            (7, 0, 0),
            (7, 0, 1),
            (9, 0, 0),
        ];
        let mut col = LevelColumn::from_aos(&data, 0, data.len(), 0);
        assert_eq!(col.values(), &[4, 4, 4, 7, 7, 9]);
        // From the level start: all distinct keys.
        assert_eq!(col.distinct_run(0), &[4, 7, 9]);
        // From a cursor parked inside the run of 7s: keys >= 7.
        assert_eq!(col.distinct_run(4), &[7, 9]);
        // From a cursor on the first 4 (still start-aligned by key): unchanged.
        assert_eq!(col.distinct_run(1), &[4, 7, 9]);
    }
}
