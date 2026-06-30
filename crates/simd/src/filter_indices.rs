//! `filter_indices_eq`: positions where `values[i] == needle`, as u32 indices.
//! The scan+index-compact primitive behind the storage partition scan
//! (SPEC-12 F2). Output indices are appended in ascending order.

use crate::dispatch::{forced_isa, Isa};
use std::sync::OnceLock;

/// Append the indices `i` (as `u32`, ascending) where `values[i] == needle`.
/// Dispatched. `values.len()` must fit in `u32` (debug-asserted).
pub fn filter_indices_eq(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    debug_assert!(values.len() <= u32::MAX as usize, "index exceeds u32");
    (dispatch())(values, needle, out)
}

type Fn_ = fn(&[u64], u64, &mut Vec<u32>);

fn dispatch() -> Fn_ {
    // A forced ISA (tests/benches) must take effect on every call, so bypass
    // the cache while a force is active. Production never forces: one
    // thread-local read, then the cached fn pointer.
    if forced_isa().is_some() {
        return resolve();
    }
    static CACHE: OnceLock<Fn_> = OnceLock::new();
    *CACHE.get_or_init(resolve)
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar,
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
            scalar
        }
    }
}

fn scalar(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    for (i, &v) in values.iter().enumerate() {
        if v == needle {
            out.push(i as u32);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    unsafe { avx2(values, needle, out) }
}

/// 4-lane (u64) equality compare → 4-bit movemask → append the set-bit
/// positions in ascending order. Tail is scalar. Differential-proven equal to
/// `scalar`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    use std::arch::x86_64::*;
    let n = values.len();
    let needle_v = _mm256_set1_epi64x(needle as i64);
    let mut i = 0usize;
    while i + 4 <= n {
        let chunk = _mm256_loadu_si256(values.as_ptr().add(i) as *const __m256i);
        let eq = _mm256_cmpeq_epi64(chunk, needle_v);
        let mask = _mm256_movemask_pd(_mm256_castsi256_pd(eq)) as u32;
        let mut m = mask;
        while m != 0 {
            let lane = m.trailing_zeros() as usize;
            out.push((i + lane) as u32);
            m &= m - 1;
        }
        i += 4;
    }
    while i < n {
        if *values.get_unchecked(i) == needle {
            out.push(i as u32);
        }
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
fn neon_safe(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    // Safety: `resolve` returns this pointer only after proving neon present.
    unsafe { neon(values, needle, out) }
}

/// 2-lane (u64) equality compare (`vceqq_u64`) against a broadcast needle;
/// extract the two lane masks and append the set-bit positions (`i`, then
/// `i + 1`) in ascending order. Tail is scalar. Differential-proven equal to
/// `scalar`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(values: &[u64], needle: u64, out: &mut Vec<u32>) {
    use std::arch::aarch64::*;
    let n = values.len();
    let needle_v = vdupq_n_u64(needle);
    let mut i = 0usize;
    while i + 2 <= n {
        let chunk = vld1q_u64(values.as_ptr().add(i));
        let eq = vceqq_u64(chunk, needle_v); // all-ones lane where chunk == needle
        if vgetq_lane_u64(eq, 0) != 0 {
            out.push(i as u32);
        }
        if vgetq_lane_u64(eq, 1) != 0 {
            out.push((i + 1) as u32);
        }
        i += 2;
    }
    while i < n {
        if *values.get_unchecked(i) == needle {
            out.push(i as u32);
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(values: &[u64], needle: u64) {
        let mut want = Vec::new();
        scalar(values, needle, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || filter_indices_eq(values, needle, &mut got));
            assert_eq!(got, want, "{isa:?}");
        }
    }

    fn forced_paths() -> Vec<Isa> {
        #[allow(unused_mut)]
        let mut v = vec![Isa::Scalar];
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                v.push(Isa::Avx2);
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            if std::arch::is_aarch64_feature_detected!("neon") {
                v.push(Isa::Neon);
            }
        }
        v
    }

    #[test]
    fn basic_and_edges() {
        let v: Vec<u64> = (0..50).map(|x| x % 5).collect();
        check(&v, 3);
        check(&v, 0);
        check(&v, 99); // no match
        check(&[], 1); // empty
        check(&[7], 7); // single element
        check(&[7], 8);
        check(&[0u64, 0, 0], 0); // all-equal, value 0
        check(&[u64::MAX, u64::MAX, u64::MAX], u64::MAX); // all-equal, value MAX
                                                          // dense matches across a wide block boundary (odd length -> wide + tail)
        let all3 = vec![3u64; 17];
        check(&all3, 3);
    }
}
