//! `lower_bound`: first index `>= value` in a non-decreasing slice.
//! Galloping (exponential) probe narrows the window, then a SIMD block
//! compare finishes it. Scalar oracle = `slice::partition_point`.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
#[cfg(not(test))]
use std::sync::OnceLock;

/// First index `i` in `haystack` with `haystack[i] >= value`, assuming
/// `haystack` is non-decreasing. Equivalent to
/// `haystack.partition_point(|&x| x < value)`.
pub fn lower_bound(haystack: &[u64], value: u64) -> usize {
    (dispatch())(haystack, value)
}

type Fn_ = fn(&[u64], u64) -> usize;

fn dispatch() -> Fn_ {
    #[cfg(test)]
    {
        resolve()
    }
    #[cfg(not(test))]
    {
        static CACHE: OnceLock<Fn_> = OnceLock::new();
        *CACHE.get_or_init(resolve)
    }
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::lower_bound,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            scalar::lower_bound
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(haystack: &[u64], value: u64) -> usize {
    // Safety: `resolve` returns this pointer only after proving avx2 present.
    unsafe { avx2(haystack, value) }
}

/// Galloping probe to bound the window to ≤ one cache line, then a linear
/// SIMD scan of four `u64` lanes per step. Returns the same index as the
/// scalar oracle for all non-decreasing inputs.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(haystack: &[u64], value: u64) -> usize {
    use std::arch::x86_64::*;
    // Gallop: find a window [lo, hi) of size <= 64 containing the boundary.
    let n = haystack.len();
    if n == 0 {
        return 0;
    }
    let mut lo = 0usize;
    let mut step = 1usize;
    while lo + step < n && *haystack.get_unchecked(lo + step) < value {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(n);
    // Linear SIMD scan of [lo, hi): broadcast `value`, compare 4 lanes/step,
    // stop at the first lane >= value.
    let needle = _mm256_set1_epi64x(value as i64);
    let mut i = lo;
    while i + 4 <= hi {
        let chunk = _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i);
        // x < value  <=>  (x ^ MIN) < (value ^ MIN) signed; cmpgt is signed,
        // so bias both operands by 2^63 to get an unsigned compare.
        let bias = _mm256_set1_epi64x(i64::MIN);
        let lt = _mm256_cmpgt_epi64(
            _mm256_xor_si256(needle, bias),
            _mm256_xor_si256(chunk, bias),
        ); // lane = 0xFFFF.. where chunk[lane] < value
        let mask = _mm256_movemask_pd(_mm256_castsi256_pd(lt)) as u32; // 4 bits
        if mask != 0b1111 {
            // First lane where chunk >= value is the first cleared bit.
            return i + mask.trailing_ones() as usize;
        }
        i += 4;
    }
    // Tail: scalar.
    while i < hi && *haystack.get_unchecked(i) < value {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(h: &[u64], v: u64) {
        let expect = scalar::lower_bound(h, v);
        with_forced_isa(Isa::Scalar, || assert_eq!(lower_bound(h, v), expect));
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            with_forced_isa(Isa::Avx2, || {
                assert_eq!(lower_bound(h, v), expect, "avx2 path")
            });
        }
    }

    #[test]
    fn boundaries() {
        let h: Vec<u64> = (0..100u64).map(|x| x * 2).collect();
        for v in [0, 1, 2, 99, 100, 198, 199, 200] {
            check(&h, v);
        }
        check(&[], 5);
        check(&[7], 7);
        check(&[7], 8);
    }
}
