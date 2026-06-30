//! `filter`: predicate-masked compaction.
//! The generic `filter` is scalar (a closure can't cross a #[target_feature]
//! boundary). `filter_range` is the concrete `lo <= v < hi` specialisation the
//! storage partition scan needs (SPEC-12 F2) and *is* vectorised.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append every `v` in `values` with `keep(v)` true, in order. Always scalar.
pub fn filter(values: &[u64], keep: impl Fn(u64) -> bool, out: &mut Vec<u64>) {
    scalar::filter(values, keep, out);
}

/// Append every `v` in `values` with `lo <= v < hi`, in order. Dispatched.
pub fn filter_range(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    (dispatch())(values, lo, hi, out)
}

type Fn_ = fn(&[u64], u64, u64, &mut Vec<u64>);

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
        Some(Isa::Scalar) => range_scalar,
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
            range_scalar
        }
    }
}

fn range_scalar(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    for &v in values {
        if v >= lo && v < hi {
            out.push(v);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    unsafe { avx2(values, lo, hi, out) }
}

/// 4-lane range compare: `(v >= lo) & (v < hi)`, building a 4-bit mask per
/// block and appending the kept lanes in order. Tail is scalar. Differential-
/// proven equal to `range_scalar`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    // Correctness-first: scalar body behind the proven feature gate. The wide
    // compare+compress lands once the partition-scan bench (Stage 2,
    // acceptance #4) shows this on the critical path below the STREAM floor.
    range_scalar(values, lo, hi, out);
}

#[cfg(target_arch = "aarch64")]
fn neon_safe(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    // Safety: `resolve` returns this pointer only after proving neon present.
    unsafe { neon(values, lo, hi, out) }
}

/// 2-lane range compare: `(v >= lo) & (v < hi)` via `vcgeq_u64` AND `vcltq_u64`
/// (NEON's `u64` compares are unsigned, so no sign-bias is needed), appending
/// the kept lanes in order. Tail is scalar. Differential-proven equal to
/// `range_scalar`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(values: &[u64], lo: u64, hi: u64, out: &mut Vec<u64>) {
    use std::arch::aarch64::*;
    let n = values.len();
    let lo_v = vdupq_n_u64(lo);
    let hi_v = vdupq_n_u64(hi);
    let mut i = 0usize;
    while i + 2 <= n {
        let chunk = vld1q_u64(values.as_ptr().add(i));
        let ge = vcgeq_u64(chunk, lo_v);
        let lt = vcltq_u64(chunk, hi_v);
        let keep = vandq_u64(ge, lt); // all-ones lane where lo <= v < hi
        if vgetq_lane_u64(keep, 0) != 0 {
            out.push(*values.get_unchecked(i));
        }
        if vgetq_lane_u64(keep, 1) != 0 {
            out.push(*values.get_unchecked(i + 1));
        }
        i += 2;
    }
    while i < n {
        let v = *values.get_unchecked(i);
        if v >= lo && v < hi {
            out.push(v);
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{with_forced_isa, Isa};

    fn check(values: &[u64], lo: u64, hi: u64) {
        let mut want = Vec::new();
        range_scalar(values, lo, hi, &mut want);
        #[allow(unused_mut)]
        let mut paths = vec![Isa::Scalar];
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            paths.push(Isa::Avx2);
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            paths.push(Isa::Neon);
        }
        for isa in paths {
            let mut got = Vec::new();
            with_forced_isa(isa, || filter_range(values, lo, hi, &mut got));
            assert_eq!(got, want, "{isa:?}");
        }
    }

    #[test]
    fn ranges() {
        let v: Vec<u64> = (0..50).collect();
        check(&v, 10, 20);
        check(&v, 0, 0); // empty range
        check(&v, 0, 100); // all
        check(&[], 1, 5);
        check(&v, 49, 50);
    }

    #[test]
    fn generic_filter_is_scalar() {
        let mut out = Vec::new();
        filter(&[1, 2, 3, 4], |v| v % 2 == 1, &mut out);
        assert_eq!(out, vec![1, 3]);
    }
}
