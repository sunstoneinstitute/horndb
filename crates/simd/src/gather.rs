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
/// winner. Only an AVX2 kernel exists for `gather` (NEON has no gather).
fn choose() -> (Isa, Fn_) {
    #[allow(unused_mut)]
    let mut candidates: Vec<(Isa, Fn_)> = vec![(Isa::Scalar, scalar::gather)];
    #[cfg(target_arch = "x86_64")]
    if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
        candidates.push((Isa::Avx2, avx2_safe));
    }

    if !crate::dispatch::autotune_enabled() {
        return *candidates.last().expect("scalar baseline always present");
    }

    // Deterministic L2-resident base + a strided index set (no `rand`).
    const N: u64 = 4096;
    let base: Vec<u64> = (0..N).map(|x| x * 3).collect();
    let indices: Vec<u32> = (0..N as u32)
        .map(|i| i.wrapping_mul(2654435761) % N as u32)
        .collect();
    let mut out: Vec<u64> = Vec::with_capacity(indices.len());
    crate::calibrate::pick(&candidates, |f| {
        out.clear();
        f(&base, &indices, &mut out);
        core::hint::black_box(&out);
    })
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::gather,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            // No NEON arm on aarch64: NEON has no gather instruction (no
            // `vpgatherqq` equivalent), so a vectorised indexed load decomposes
            // into the same scalar loads plus lane-assembly overhead and cannot
            // beat the scalar path. Scalar is optimal here.
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
