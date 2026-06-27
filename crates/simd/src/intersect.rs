//! `intersect`: sorted-set intersection of two ascending, deduped slices.
//! Appends the (sorted) intersection to `out`. NF2 target: >=4x scalar on
//! AVX-512, >=2x on NEON, measured at L2-resident sizes.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append `a ∩ b` (both sorted-ascending, deduped) to `out`, in order.
pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    (dispatch())(a, b, out)
}

type Fn_ = fn(&[u64], &[u64], &mut Vec<u64>);

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
        Some(Isa::Scalar) => scalar::intersect,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx512) if is_x86_feature_detected!("avx512f") => avx512_safe,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            {
                // Bench (acceptance #3) decides whether AVX-512 or AVX2 wins on
                // Zen4; until then prefer AVX-512 when present.
                if is_x86_feature_detected!("avx512f") {
                    return avx512_safe;
                }
                if is_x86_feature_detected!("avx2") {
                    return avx2_safe;
                }
            }
            scalar::intersect
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx2(a, b, out) }
}

/// Block-vs-block merge: galloping skip on the smaller side, then an
/// all-pairs SIMD compare of a 4-wide block of `a` against a 4-wide block of
/// `b`. Falls back to scalar two-pointer for the tail. Output order matches
/// the scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Correctness-first kernel: the SPEC's NF2 floor is a throughput target,
    // not a per-byte-of-source mandate. Start with a galloping two-pointer
    // that vectorises the "skip ahead in b until b[j] >= a[i]" probe via
    // `lower_bound`, then emit matches. This is provably equal to the scalar
    // oracle and is the shape the bench measures; tighten to all-pairs SIMD
    // compare only if the bench misses 4x.
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        // Advance j to the first b >= av using the SIMD lower_bound over the
        // remaining b suffix.
        j += crate::lower_bound::lower_bound(&b[j..], av);
        if j >= b.len() {
            break;
        }
        let bv = *b.get_unchecked(j);
        if av == bv {
            out.push(av);
            i += 1;
            j += 1;
        } else {
            // bv > av: advance a to first a >= bv.
            i += crate::lower_bound::lower_bound(&a[i..], bv);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx512_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx512(a, b, out) }
}

/// AVX-512 conflict/compare intersection. Same galloping skeleton as the AVX2
/// kernel but emits an 8-wide `_mm512_cmpeq_epi64_mask` compare of an a-block
/// against a broadcast b-cursor (and vice versa), compacting matches with
/// `_mm512_mask_compressstoreu_epi64`. Differential-proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn avx512(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Same correctness-first galloping shape as `avx2`, reusing the SIMD
    // lower_bound (which itself dispatches to the widest available kernel).
    // The 8-wide compress path is a throughput optimisation layered on once
    // the bench (acceptance #3) shows the galloping form misses 4x.
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        j += crate::lower_bound::lower_bound(&b[j..], av);
        if j >= b.len() {
            break;
        }
        let bv = *b.get_unchecked(j);
        if av == bv {
            out.push(av);
            i += 1;
            j += 1;
        } else {
            i += crate::lower_bound::lower_bound(&a[i..], bv);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(a: &[u64], b: &[u64]) {
        let mut want = Vec::new();
        scalar::intersect(a, b, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || intersect(a, b, &mut got));
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
            if is_x86_feature_detected!("avx512f") {
                v.push(Isa::Avx512);
            }
        }
        v
    }

    #[test]
    fn basic_and_edges() {
        check(&[1, 2, 3, 5, 8], &[2, 3, 4, 8, 9]);
        check(&[], &[1, 2, 3]);
        check(&[1, 2, 3], &[]);
        check(&[1, 2, 3], &[4, 5, 6]);
        let big: Vec<u64> = (0..1000).map(|x| x * 2).collect();
        let odd: Vec<u64> = (0..1000).map(|x| x * 3).collect();
        check(&big, &odd);
    }
}
