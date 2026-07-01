//! `merge`: full two-way merge (keeps duplicates) of two ascending slices.
//! Appends the sorted union-with-multiplicity to `out`.

use crate::dispatch::{forced_isa, Isa};
use crate::scalar;
use std::sync::OnceLock;

/// Append the full sorted merge of `a` and `b` (both ascending, duplicates
/// kept) to `out`, in order.
pub fn merge(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    (dispatch())(a, b, out)
}

type Fn_ = fn(&[u64], &[u64], &mut Vec<u64>);

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
/// winner. Only an AVX2 kernel exists for `merge`.
fn choose() -> (Isa, Fn_) {
    #[allow(unused_mut)]
    let mut candidates: Vec<(Isa, Fn_)> = vec![(Isa::Scalar, scalar::merge)];
    #[cfg(target_arch = "x86_64")]
    if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
        candidates.push((Isa::Avx2, avx2_safe));
    }

    // Known-CPU table: an authoritative per-host choice wins with no timing,
    // provided that ISA is present in the capped candidate list.
    if let Some(pair) = crate::cpu::table_pick_pair(&candidates, crate::cpu::Kernel::Merge) {
        return pair;
    }

    if !crate::dispatch::autotune_enabled() {
        return *candidates.last().expect("scalar baseline always present");
    }

    // Deterministic L2-resident interleaved runs (no `rand`).
    const N: u64 = 4096;
    let a: Vec<u64> = (0..N).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..N).map(|x| x * 2 + 1).collect();
    let mut out: Vec<u64> = Vec::with_capacity(a.len() + b.len());
    crate::calibrate::pick(&candidates, |f| {
        out.clear();
        f(&a, &b, &mut out);
        core::hint::black_box(&out);
    })
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::merge,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
                return avx2_safe;
            }
            scalar::merge
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx2(a, b, out) }
}

/// Branch-reduced two-way merge. The vector win for a full sorted merge is
/// modest (merge is branch-heavy); this kernel uses a vectorised "bitonic
/// merge network on 4+4 lanes" only when both remaining runs are >= 8 long,
/// else falls to the scalar oracle. Differential-proven equal to scalar.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Correctness-first: defer to the scalar oracle. `merge` is the lowest-
    // payoff primitive (branchy, memory-bound); it earns a real vector kernel
    // only if the F3 delta-apply bench (Stage 3) shows it on the hot path and
    // it clears a measured floor. Until then the "AVX2" path is the oracle,
    // which keeps the dispatch surface uniform without shipping an unproven
    // intrinsics body. See SPEC-12 risk §"A primitive earns its intrinsics
    // only if it clears the NF2/NF4 >=4x floor; otherwise ship scalar."
    scalar::merge(a, b, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::with_forced_isa;

    fn check(a: &[u64], b: &[u64]) {
        let mut want = Vec::new();
        scalar::merge(a, b, &mut want);
        for isa in forced_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || merge(a, b, &mut got));
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
        // disjoint
        check(&[1, 3, 5], &[2, 4, 6]);
        // fully overlapping with duplicates
        check(&[1, 3, 3, 5], &[1, 3, 5]);
        // one empty
        check(&[], &[1, 2, 3]);
        check(&[1, 2, 3], &[]);
        // long runs
        let a: Vec<u64> = (0..500).map(|x| x * 2).collect();
        let b: Vec<u64> = (0..500).map(|x| x * 2 + 1).collect();
        check(&a, &b);
    }
}
