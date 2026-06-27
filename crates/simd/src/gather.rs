//! `gather`: indexed load — append `base[indices[i]]` for each `i`, in order.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append `base[indices[i]]` for each `i`, in order. Every index must be
/// `< base.len()` (debug-asserted here so it fires on every dispatch path).
pub fn gather(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    debug_assert!(
        indices.iter().all(|&i| (i as usize) < base.len()),
        "gather index out of bounds"
    );
    (dispatch())(base, indices, out)
}

type Fn_ = fn(&[u64], &[u32], &mut Vec<u64>);

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
        Some(Isa::Scalar) => scalar::gather,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            scalar::gather
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    unsafe { avx2(base, indices, out) }
}

/// Vectorised indexed load using `vpgatherqq`. Loads 4 `u64`s per step from
/// `base` at the `u32` indices, appends them in order. Indices must be in
/// bounds (debug-asserted by the wrapper before dispatch). Differential-
/// proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(base: &[u64], indices: &[u32], out: &mut Vec<u64>) {
    use std::arch::x86_64::*;
    let start = out.len();
    out.reserve(indices.len());
    let mut k = 0usize;
    while k + 4 <= indices.len() {
        let idx = _mm_loadu_si128(indices.as_ptr().add(k) as *const __m128i);
        let g = _mm256_i32gather_epi64::<8>(base.as_ptr() as *const i64, idx);
        let mut tmp = [0u64; 4];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, g);
        out.extend_from_slice(&tmp);
        k += 4;
    }
    while k < indices.len() {
        out.push(*base.get_unchecked(*indices.get_unchecked(k) as usize));
        k += 1;
    }
    debug_assert_eq!(out.len(), start + indices.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(base: &[u64], indices: &[u32]) {
        let mut want = Vec::new();
        scalar::gather(base, indices, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || gather(base, indices, &mut got));
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
        v
    }

    #[test]
    fn basic_and_edges() {
        let base: Vec<u64> = (0..20u64).map(|x| x * 10).collect();
        // random in-bounds indices (more than 4 to exercise the wide path)
        check(&base, &[3, 0, 19, 7, 7, 12, 1, 5, 9]);
        // empty indices
        check(&base, &[]);
        // single element
        check(&base, &[11]);
        // indices that repeat
        check(&base, &[2, 2, 2, 2, 2]);
    }
}
