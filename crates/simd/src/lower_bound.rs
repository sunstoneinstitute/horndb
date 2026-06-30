//! `lower_bound`: first index `>= value` in a non-decreasing slice.
//! Galloping (exponential) probe narrows the window, then a SIMD block
//! compare finishes it. Scalar oracle = `slice::partition_point`.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// First index `i` in `haystack` with `haystack[i] >= value`, assuming
/// `haystack` is non-decreasing. Equivalent to
/// `haystack.partition_point(|&x| x < value)`.
pub fn lower_bound(haystack: &[u64], value: u64) -> usize {
    (dispatch())(haystack, value)
}

type Fn_ = fn(&[u64], u64) -> usize;

/// The cached `(chosen ISA, kernel)` pair, calibrated once on first call.
fn cached() -> (Isa, Fn_) {
    static CACHE: OnceLock<(Isa, Fn_)> = OnceLock::new();
    *CACHE.get_or_init(choose)
}

fn dispatch() -> Fn_ {
    // A forced ISA (tests/benches) must take effect on every call, so bypass
    // the cache while a force is active. Production never forces: one
    // thread-local read, then the cached fn pointer.
    if forced_isa().is_some() {
        return resolve();
    }
    cached().1
}

/// Prime the cached kernel (paying any calibration cost now). Called by
/// [`crate::init`].
pub(crate) fn prime() {
    let _ = cached();
}

/// The ISA of the cached kernel (calibrating on first call if needed).
pub(crate) fn chosen() -> Isa {
    cached().0
}

/// Build the host-supported, cap-allowed candidate list and pick the kernel:
/// the static widest preference when auto-tune is off, else the micro-calibrated
/// winner. No AVX-512 kernel exists for `lower_bound`.
fn choose() -> (Isa, Fn_) {
    #[allow(unused_mut)]
    let mut candidates: Vec<(Isa, Fn_)> = vec![(Isa::Scalar, scalar::lower_bound)];
    #[cfg(target_arch = "x86_64")]
    if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
        candidates.push((Isa::Avx2, avx2_safe));
    }
    #[cfg(target_arch = "aarch64")]
    if crate::dispatch::allows(Isa::Neon) && std::arch::is_aarch64_feature_detected!("neon") {
        candidates.push((Isa::Neon, neon_safe));
    }

    if !crate::dispatch::autotune_enabled() {
        return *candidates.last().expect("scalar baseline always present");
    }

    // Deterministic L2-resident sorted run + a mid needle (no `rand`).
    const N: u64 = 4096;
    let h: Vec<u64> = (0..N).map(|x| x * 2).collect();
    let needle = N; // ~midpoint of the value range
    crate::calibrate::pick(&candidates, |f| {
        core::hint::black_box(f(&h, needle));
    })
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::lower_bound,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        #[cfg(target_arch = "aarch64")]
        Some(Isa::Neon) if std::arch::is_aarch64_feature_detected!("neon") => neon_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            #[cfg(target_arch = "aarch64")]
            if crate::dispatch::allows(Isa::Neon) && std::arch::is_aarch64_feature_detected!("neon")
            {
                return neon_safe;
            }
            scalar::lower_bound
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn neon_safe(haystack: &[u64], value: u64) -> usize {
    // Safety: `resolve` returns this pointer only after proving neon present.
    unsafe { neon(haystack, value) }
}

/// Galloping probe to bound the window, then a 2-lane (`uint64x2_t`) linear
/// SIMD scan: broadcast `value`, compare two `u64` lanes per step with
/// `vcltq_u64`, and stop at the first lane `>= value` (found by lane
/// extraction). NEON's `u64` compare is unsigned, so no sign-bias is needed
/// (unlike the AVX2 kernel). Returns the same index as the scalar oracle for
/// all non-decreasing inputs.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(haystack: &[u64], value: u64) -> usize {
    use std::arch::aarch64::*;
    let n = haystack.len();
    if n == 0 {
        return 0;
    }
    // Gallop: find a window [lo, hi) containing the boundary.
    let mut lo = 0usize;
    let mut step = 1usize;
    while lo + step < n && *haystack.get_unchecked(lo + step) < value {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(n);
    // Linear NEON scan of [lo, hi): broadcast `value`, compare 2 lanes/step,
    // stop at the first lane >= value. `vcltq_u64` yields all-ones in a lane
    // where `chunk[lane] < value`; the slice is non-decreasing, so the per-lane
    // mask is monotone (1,1 / 1,0 / 0,0) and the first zero lane is the answer.
    let needle = vdupq_n_u64(value);
    let mut i = lo;
    while i + 2 <= hi {
        let chunk = vld1q_u64(haystack.as_ptr().add(i));
        let lt = vcltq_u64(chunk, needle);
        if vgetq_lane_u64(lt, 0) == 0 {
            return i;
        }
        if vgetq_lane_u64(lt, 1) == 0 {
            return i + 1;
        }
        i += 2;
    }
    // Tail: scalar.
    while i < hi && *haystack.get_unchecked(i) < value {
        i += 1;
    }
    i
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
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            with_forced_isa(Isa::Neon, || {
                assert_eq!(lower_bound(h, v), expect, "neon path")
            });
        }
    }

    #[test]
    fn boundaries() {
        // 100 elements: exercises both the 2-lane wide scan and the scalar tail.
        let h: Vec<u64> = (0..100u64).map(|x| x * 2).collect();
        for v in [0, 1, 2, 99, 100, 198, 199, 200] {
            check(&h, v);
        }
        check(&[], 5); // empty
        check(&[7], 7); // single element
        check(&[7], 8);
        check(&[0, 0, 0], 0); // all-zero block
                              // u64::MAX boundary (and an odd length so the tail runs).
        let m: Vec<u64> = vec![0, 1, 2, u64::MAX, u64::MAX];
        for v in [0, 3, u64::MAX] {
            check(&m, v);
        }
    }
}
