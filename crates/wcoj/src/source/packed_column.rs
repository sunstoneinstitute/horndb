//! `PackedColumn` — a compact, random-access encoding of one `u64` column.
//!
//! The column is split into fixed-size blocks. Each block stores a
//! frame-of-reference base (the block minimum) and the minimal bit width `w`
//! needed to represent `value - base` for every value in the block; the
//! residuals are bit-packed LSB-first into a shared `u64` word stream, with
//! each block starting on a word boundary so `get` never needs the block's
//! global bit offset. A constant block uses `w = 0` and stores nothing.
//!
//! This is the building block for `CompressedTripleSource`: the WCOJ trie
//! cursor reads column values via [`PackedColumn::get`] and narrows ranges via
//! [`PackedColumn::lower_bound`] / [`PackedColumn::upper_bound`], so the column
//! never needs to be fully materialised as dense `u64`s.

/// Values per block. 256 keeps per-block metadata overhead negligible while
/// still letting frame-of-reference exploit local value locality in sorted
/// columns.
const BLOCK: usize = 256;

#[derive(Clone, Copy)]
struct BlockMeta {
    /// Frame-of-reference base: the minimum value in the block.
    base: u64,
    /// Bit width of `value - base`. `0` means a constant block (no payload).
    bits: u8,
    /// Index into `words` where this block's packed residuals start.
    /// `usize` (not `u32`): a column whose packed payload exceeds `u32::MAX`
    /// words (~34 GiB) is plausible on HornDB's unified-memory target, and a
    /// truncating offset would silently corrupt every later block.
    word_offset: usize,
}

/// A compact, random-access encoding of one `u64` column.
pub struct PackedColumn {
    len: usize,
    blocks: Vec<BlockMeta>,
    words: Vec<u64>,
}

#[inline]
fn bits_for(max_delta: u64) -> u8 {
    if max_delta == 0 {
        0
    } else {
        (64 - max_delta.leading_zeros()) as u8
    }
}

impl PackedColumn {
    /// Encode `values` (any order; sorted not required for correctness, but
    /// frame-of-reference compresses sorted/locally-clustered data best).
    pub fn from_slice(values: &[u64]) -> Self {
        let mut blocks = Vec::with_capacity(values.len().div_ceil(BLOCK));
        let mut words: Vec<u64> = Vec::new();
        for chunk in values.chunks(BLOCK) {
            let base = *chunk.iter().min().expect("non-empty chunk");
            let max_delta = chunk.iter().map(|v| v - base).max().unwrap();
            let bits = bits_for(max_delta);
            let word_offset = words.len();
            blocks.push(BlockMeta {
                base,
                bits,
                word_offset,
            });
            if bits == 0 {
                continue;
            }
            // Reserve enough words for `chunk.len() * bits` bits, then write.
            let total_bits = chunk.len() * bits as usize;
            let n_words = total_bits.div_ceil(64);
            words.resize(word_offset + n_words, 0);
            for (i, v) in chunk.iter().enumerate() {
                let delta = v - base;
                let bit_index = i * bits as usize;
                let w = word_offset + bit_index / 64;
                let off = bit_index % 64;
                words[w] |= delta << off;
                if off + bits as usize > 64 {
                    words[w + 1] |= delta >> (64 - off);
                }
            }
        }
        Self {
            len: values.len(),
            blocks,
            words,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Decode the value at index `i`.
    ///
    /// `i` must be `< len`. In release builds an out-of-bounds `i` that still
    /// falls inside the final (partially-filled) block reads zero-padded tail
    /// bits and returns a garbage value rather than panicking, so callers must
    /// respect the bound; a `debug_assert!` catches violations in tests.
    #[inline]
    pub fn get(&self, i: usize) -> u64 {
        debug_assert!(i < self.len, "index {i} out of bounds (len {})", self.len);
        let meta = &self.blocks[i / BLOCK];
        if meta.bits == 0 {
            return meta.base;
        }
        let bits = meta.bits as usize;
        let bit_index = (i % BLOCK) * bits;
        let w = meta.word_offset + bit_index / 64;
        let off = bit_index % 64;
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let mut v = self.words[w] >> off;
        if off + bits > 64 {
            v |= self.words[w + 1] << (64 - off);
        }
        meta.base + (v & mask)
    }

    /// Last value of block `b`, i.e. the value at the highest index the block
    /// holds. Used to bisect over blocks: for a non-decreasing column the
    /// per-block last values are themselves non-decreasing.
    #[inline]
    fn block_last(&self, b: usize) -> u64 {
        let end = ((b + 1) * BLOCK).min(self.len);
        self.get(end - 1)
    }

    /// Decode block `b` into `scratch` (cleared then filled). Returns the
    /// absolute start index of the block.
    #[inline]
    fn decode_block(&self, b: usize, scratch: &mut Vec<u64>) -> usize {
        let start = b * BLOCK;
        let end = ((b + 1) * BLOCK).min(self.len);
        scratch.clear();
        for i in start..end {
            scratch.push(self.get(i));
        }
        start
    }

    /// First index in `[lo, hi)` whose value is `>= value`, assuming the column
    /// is non-decreasing across that range. Mirrors `slice::partition_point(|x| x < value)`.
    ///
    /// Bisects over blocks (by each block's last value) to the single block that
    /// can hold the boundary, decodes just that block into a contiguous scratch
    /// buffer, and finishes with the SIMD `lower_bound`. Equivalent to
    /// [`Self::lower_bound_scalar`] — see the `lower_bound_matches_scalar`
    /// proptest.
    #[inline]
    pub fn lower_bound(&self, lo: usize, hi: usize, value: u64) -> usize {
        if lo >= hi {
            return lo;
        }
        let first_block = lo / BLOCK;
        let last_block = (hi - 1) / BLOCK;
        // Smallest block `b` in `[first_block, last_block]` whose last value is
        // `>= value`; the boundary, if any, lies inside it. If every block's
        // last value is `< value`, this settles on `last_block` and the SIMD
        // finish below returns `hi`.
        let mut blo = first_block;
        let mut bhi = last_block;
        while blo < bhi {
            let mid = blo + (bhi - blo) / 2;
            if self.block_last(mid) < value {
                blo = mid + 1;
            } else {
                bhi = mid;
            }
        }
        let b = blo;
        // Decode the owning block and SIMD-finish within its active sub-range.
        let mut scratch: Vec<u64> = Vec::with_capacity(BLOCK);
        let block_start = self.decode_block(b, &mut scratch);
        let sub_lo = lo.max(block_start);
        let sub_hi = hi.min(block_start + scratch.len());
        let rel_lo = sub_lo - block_start;
        let rel_hi = sub_hi - block_start;
        let off = horndb_simd::lower_bound(&scratch[rel_lo..rel_hi], value);
        sub_lo + off
    }

    /// Scalar bisection lower-bound, retained as the correctness oracle for the
    /// SIMD [`Self::lower_bound`].
    #[inline]
    pub fn lower_bound_scalar(&self, lo: usize, hi: usize, value: u64) -> usize {
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.get(mid) < value {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// First index in `[lo, hi)` whose value is `> value`, assuming the column
    /// is non-decreasing across that range. Mirrors `slice::partition_point(|x| x <= value)`.
    #[inline]
    pub fn upper_bound(&self, lo: usize, hi: usize, value: u64) -> usize {
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.get(mid) <= value {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Heap bytes used by this column (payload + per-block metadata).
    pub fn heap_bytes(&self) -> usize {
        self.words.len() * std::mem::size_of::<u64>()
            + self.blocks.len() * std::mem::size_of::<BlockMeta>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn lower_bound_matches_scalar(mut vals: Vec<u64>, value: u64) {
            vals.sort_unstable();
            let col = PackedColumn::from_slice(&vals);
            let n = vals.len();
            let want = vals.partition_point(|&x| x < value);
            prop_assert_eq!(col.lower_bound(0, n, value), want);
            prop_assert_eq!(col.lower_bound_scalar(0, n, value), want);
        }

        #[test]
        fn lower_bound_matches_scalar_subrange(
            mut vals: Vec<u64>,
            value: u64,
            a: usize,
            b: usize,
        ) {
            prop_assume!(!vals.is_empty());
            vals.sort_unstable();
            let n = vals.len();
            let lo = a % n;
            let hi = lo + (b % (n - lo + 1));
            let col = PackedColumn::from_slice(&vals);
            let want = lo + vals[lo..hi].partition_point(|&x| x < value);
            prop_assert_eq!(col.lower_bound(lo, hi, value), want);
        }
    }

    fn roundtrip(values: &[u64]) {
        let col = PackedColumn::from_slice(values);
        assert_eq!(col.len(), values.len());
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(col.get(i), v, "mismatch at index {i}");
        }
    }

    #[test]
    fn roundtrip_empty() {
        roundtrip(&[]);
    }

    #[test]
    fn roundtrip_single() {
        roundtrip(&[42]);
    }

    #[test]
    fn roundtrip_constant_block() {
        roundtrip(&vec![7u64; 300]);
    }

    #[test]
    fn roundtrip_monotonic_multiblock() {
        let v: Vec<u64> = (0..1000u64).map(|i| i * 3 + 5).collect();
        roundtrip(&v);
    }

    #[test]
    fn roundtrip_random_with_large_values() {
        // Values needing wide bit widths, including ones that force a 64-bit
        // residual (base 0, value u64::MAX) and cross-word reads.
        let v = vec![0u64, u64::MAX, 1, u64::MAX - 1, 1 << 40, (1 << 40) + 7];
        roundtrip(&v);
    }

    #[test]
    fn roundtrip_full_block_boundary() {
        // Exactly BLOCK and BLOCK+1 elements exercise the block-boundary path.
        roundtrip(&(0..BLOCK as u64).collect::<Vec<_>>());
        roundtrip(&(0..(BLOCK as u64 + 1)).collect::<Vec<_>>());
    }

    #[test]
    fn lower_upper_bound_match_partition_point() {
        let v: Vec<u64> = (0..500u64).map(|i| (i / 3) * 2).collect(); // sorted, with dups
        let col = PackedColumn::from_slice(&v);
        for target in [0u64, 1, 2, 4, 332, 999] {
            let lb = col.lower_bound(0, v.len(), target);
            let expect_lb = v.partition_point(|&x| x < target);
            assert_eq!(lb, expect_lb, "lower_bound target={target}");
            let ub = col.upper_bound(0, v.len(), target);
            let expect_ub = v.partition_point(|&x| x <= target);
            assert_eq!(ub, expect_ub, "upper_bound target={target}");
        }
    }

    #[test]
    fn bounds_respect_subrange() {
        let v: Vec<u64> = (0..100u64).collect();
        let col = PackedColumn::from_slice(&v);
        assert_eq!(col.lower_bound(10, 20, 5), 10); // clamped to lo
        assert_eq!(col.lower_bound(10, 20, 15), 15);
        assert_eq!(col.lower_bound(10, 20, 99), 20); // clamped to hi
    }
}
