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
    /// Column values for rows `lo..hi`, contiguous.
    values: Vec<TermId>,
    /// The `lo` the column was built from, so callers map absolute row indices.
    base: usize,
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
        Self { values, base: lo }
    }

    /// First absolute row index in `[start_abs, base+len)` whose value is
    /// `>= value`, using the SIMD lower_bound. Returns an absolute
    /// (data-relative) index.
    pub(crate) fn lower_bound_from(&self, start_abs: usize, value: TermId) -> usize {
        let start_rel = start_abs - self.base;
        let off = horndb_simd::lower_bound(&self.values[start_rel..], value);
        start_abs + off
    }

    pub(crate) fn values(&self) -> &[TermId] {
        &self.values
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
}
