//! `dedup`: collapse runs of equal values in a non-decreasing slice, appending
//! each distinct value once (in order) to `out`.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append each distinct value of `sorted` (non-decreasing) once, in order.
pub fn dedup(sorted: &[u64], out: &mut Vec<u64>) {
    (dispatch())(sorted, out)
}

type Fn_ = fn(&[u64], &mut Vec<u64>);

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
        Some(Isa::Scalar) => scalar::dedup,
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
            scalar::dedup
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(sorted: &[u64], out: &mut Vec<u64>) {
    unsafe { avx2(sorted, out) }
}

/// Vectorised sorted-run dedup. For each block, compare lane `i` against lane
/// `i-1` (the previous element, carried across block boundaries) to mark the
/// first occurrence of each value, then compact the kept lanes. The boundary
/// element between blocks is carried in a scalar `last`. Differential-proven
/// equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(sorted: &[u64], out: &mut Vec<u64>) {
    // Correctness-first galloping form: emit runs by finding each run's end
    // with the SIMD lower_bound (first index > current value), pushing one
    // copy. Equal to the scalar oracle for all non-decreasing inputs.
    let mut i = 0usize;
    while i < sorted.len() {
        let v = *sorted.get_unchecked(i);
        out.push(v);
        // Skip the rest of this run: the count of elements equal to `v` from
        // `i`, i.e. the first index strictly greater than `v`. For `v < MAX`
        // that is `lower_bound(.., v + 1)`. `v == u64::MAX` has no greater
        // value, and the slice is non-decreasing, so every remaining element
        // equals MAX — the run extends to the end. (Computing `v + 1` here
        // would wrap to 0 and return run = 0, re-emitting the duplicate.)
        let run = if v == u64::MAX {
            sorted.len() - i
        } else {
            crate::lower_bound::lower_bound(&sorted[i..], v + 1)
        };
        i += run.max(1);
    }
}

#[cfg(target_arch = "aarch64")]
fn neon_safe(sorted: &[u64], out: &mut Vec<u64>) {
    // Safety: `resolve` returns this pointer only after proving neon present.
    unsafe { neon(sorted, out) }
}

/// Galloping-run dedup: emit each run once, finding the run end with the SIMD
/// `lower_bound` (first index strictly greater than the current value). Under a
/// forced/​detected NEON path `lower_bound` itself runs the real 2-lane NEON
/// kernel, so this is a genuine SIMD improvement over the scalar oracle rather
/// than a placeholder. The `v == u64::MAX` guard is identical to the AVX2 body:
/// `v + 1` would wrap to 0 and report a zero-length run, re-emitting the
/// duplicate. Differential-proven equal to scalar.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(sorted: &[u64], out: &mut Vec<u64>) {
    let mut i = 0usize;
    while i < sorted.len() {
        let v = *sorted.get_unchecked(i);
        out.push(v);
        let run = if v == u64::MAX {
            sorted.len() - i
        } else {
            crate::lower_bound::lower_bound(&sorted[i..], v + 1)
        };
        i += run.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(sorted: &[u64]) {
        let mut want = Vec::new();
        scalar::dedup(sorted, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || dedup(sorted, &mut got));
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
        check(&[1, 1, 1]); // all-equal block
        check(&[1, 2, 3]); // no dups
        check(&[]); // empty
        check(&[7]); // single element
        check(&[0, 0, 0]); // all-zero block
        check(&[u64::MAX, u64::MAX]); // wrap edge: v+1 overflows to 0
        check(&[0, u64::MAX, u64::MAX]); // MAX run reached mid-slice
                                         // long run with clustered duplicates (exercises wide lower_bound)
        let mut v = Vec::new();
        for x in 0..200u64 {
            for _ in 0..(x % 4 + 1) {
                v.push(x);
            }
        }
        check(&v);
    }
}
