//! `intersect`: sorted-set intersection of two ascending, deduped slices.
//! Appends the (sorted) intersection to `out`. NF2 target: >=4x scalar on
//! AVX-512, >=2x on NEON, measured at L2-resident sizes.

use crate::dispatch::{forced_isa, Isa};
use crate::lower_bound::lower_bound;
use crate::scalar;
use std::sync::OnceLock;

/// Skew threshold above which galloping beats the block-vs-block SIMD kernels.
///
/// The block kernels are O(|large|) — they walk the entire larger side — while
/// galloping is O(|small|·log|large|). Measured on the hornbench Zen4 host: at
/// `hi/lo` ≈ 16 the two break even (block ~0.4× at parity), below it the block
/// kernel's balanced throughput wins, and above it galloping wins by 1–3 orders
/// of magnitude on skewed shapes (e.g. 64×1_000_000 was 747× slower on block).
/// Leapfrog feeds `intersect` exactly these skewed `active_run`s, so the gate
/// matters in production. Repairs the regression bisected to `ccecd5f`.
const GALLOP_RATIO: usize = 16;

/// Append `a ∩ b` (both sorted-ascending, deduped) to `out`, in order.
pub fn intersect(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // A forced ISA (tests/benches) must exercise that *exact* block kernel, so
    // bypass the size-ratio gate and route straight to it — the force is a
    // test/bench affordance to verify/measure one specific kernel, and letting
    // the gate divert it to the scalar galloping path would defeat the
    // differential proptest. The gate is a production-only optimisation.
    if forced_isa().is_some() {
        return (resolve())(a, b, out);
    }
    // Production path: skewed size ratios make the block kernel walk the whole
    // larger side; galloping is far cheaper there. Balanced inputs keep the
    // block-SIMD win.
    let lo = a.len().min(b.len());
    let hi = a.len().max(b.len());
    if hi >= GALLOP_RATIO * lo.max(1) {
        gallop(a, b, out);
    } else {
        cached().1(a, b, out);
    }
}

/// Galloping (ISA-independent) intersection for skewed inputs: walk the smaller
/// side and exponential/binary-search each element in the larger side via
/// [`crate::lower_bound::lower_bound`], advancing a monotone cursor.
/// O(|small|·log|large|) vs the block kernels' O(|large|). Inputs are
/// sorted-ascending and deduped; the result is appended in ascending order,
/// byte-identical to the scalar two-pointer oracle. Correct regardless of which
/// operand is larger.
fn gallop(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    // Iterate the smaller side ascending so the appended output stays sorted.
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut pos = 0usize; // cursor into `large`; only ever advances
    for &v in small {
        pos += lower_bound(&large[pos..], v);
        if pos >= large.len() {
            break;
        }
        if large[pos] == v {
            out.push(v);
            pos += 1; // deduped: next match is strictly after this one
        }
    }
}

type Fn_ = fn(&[u64], &[u64], &mut Vec<u64>);

/// The cached `(chosen ISA, kernel)` pair, calibrated once on first call.
fn cached() -> (Isa, Fn_) {
    static CACHE: OnceLock<(Isa, Fn_)> = OnceLock::new();
    *CACHE.get_or_init(choose)
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
/// winner.
fn choose() -> (Isa, Fn_) {
    #[allow(unused_mut)]
    let mut candidates: Vec<(Isa, Fn_)> = vec![(Isa::Scalar, scalar::intersect)];
    #[cfg(target_arch = "x86_64")]
    {
        // Push order is scalar, avx2, avx512 so "last" = the widest available,
        // matching `resolve`'s avx512-over-avx2 preference.
        if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
            candidates.push((Isa::Avx2, avx2_safe));
        }
        if crate::dispatch::allows(Isa::Avx512) && is_x86_feature_detected!("avx512f") {
            candidates.push((Isa::Avx512, avx512_safe));
        }
    }
    #[cfg(target_arch = "aarch64")]
    if crate::dispatch::allows(Isa::Neon) && std::arch::is_aarch64_feature_detected!("neon") {
        candidates.push((Isa::Neon, neon_safe));
    }

    if !crate::dispatch::autotune_enabled() {
        return *candidates.last().expect("scalar baseline always present");
    }

    let (a, b) = calib_input();
    let mut out: Vec<u64> = Vec::with_capacity(a.len());
    crate::calibrate::pick(&candidates, |f| {
        out.clear();
        f(&a, &b, &mut out);
        core::hint::black_box(&out);
    })
}

/// Deterministic L2-resident workload: two sorted, deduped runs with ~50%
/// overlap (no `rand`).
fn calib_input() -> (Vec<u64>, Vec<u64>) {
    const N: u64 = 4096;
    let a: Vec<u64> = (0..N).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..N).map(|x| x * 2 + (x % 2)).collect();
    (a, b)
}

fn resolve() -> Fn_ {
    match forced_isa() {
        Some(Isa::Scalar) => scalar::intersect,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx512) if is_x86_feature_detected!("avx512f") => avx512_safe,
        #[cfg(target_arch = "x86_64")]
        Some(Isa::Avx2) if is_x86_feature_detected!("avx2") => avx2_safe,
        #[cfg(target_arch = "aarch64")]
        Some(Isa::Neon) if std::arch::is_aarch64_feature_detected!("neon") => neon_safe,
        _ => {
            #[cfg(target_arch = "x86_64")]
            {
                // Bench (acceptance #3) decides whether AVX-512 or AVX2 wins on
                // Zen4; until then prefer AVX-512 when present. `HORNDB_SIMD_MAX_ISA`
                // caps this — e.g. `=avx2` forces the AVX2 path on a Zen4 box
                // without a rebuild if AVX-512 downclocking loses net.
                if crate::dispatch::allows(Isa::Avx512) && is_x86_feature_detected!("avx512f") {
                    return avx512_safe;
                }
                if crate::dispatch::allows(Isa::Avx2) && is_x86_feature_detected!("avx2") {
                    return avx2_safe;
                }
            }
            #[cfg(target_arch = "aarch64")]
            if crate::dispatch::allows(Isa::Neon) && std::arch::is_aarch64_feature_detected!("neon")
            {
                return neon_safe;
            }
            scalar::intersect
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn neon_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { neon(a, b, out) }
}

/// NEON sorted-set intersection (block-vs-block, `W = 2`).
///
/// While a full 2-lane block of each side remains, load `A = a[i..i+2]` and the
/// two scalars of `B = b[j..j+2]`, then do a genuine `uint64x2_t` all-pairs
/// compare: `vceqq_u64(A, dup(b0)) | vceqq_u64(A, dup(b1))` yields a per-`a`-lane
/// mask of which `a` lanes appear in the `B` block. Matched lanes are emitted in
/// lane order (inputs are sorted+deduped, so the output stays sorted). Then
/// advance the side whose block-max is smaller (both on a tie). A scalar
/// two-pointer drains the tail. Bit-identical to the scalar oracle.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    use std::arch::aarch64::*;
    const W: usize = 2;
    let (mut i, mut j) = (0usize, 0usize);
    while i + W <= a.len() && j + W <= b.len() {
        let av = vld1q_u64(a.as_ptr().add(i));
        let b0 = *b.get_unchecked(j);
        let b1 = *b.get_unchecked(j + 1);
        // All-pairs: which `a` lanes equal b0 or b1.
        let eq = vorrq_u64(
            vceqq_u64(av, vdupq_n_u64(b0)),
            vceqq_u64(av, vdupq_n_u64(b1)),
        );
        if vgetq_lane_u64::<0>(eq) != 0 {
            out.push(*a.get_unchecked(i));
        }
        if vgetq_lane_u64::<1>(eq) != 0 {
            out.push(*a.get_unchecked(i + 1));
        }
        let a_max = *a.get_unchecked(i + W - 1);
        let b_max = *b.get_unchecked(j + W - 1);
        if a_max <= b_max {
            i += W;
        }
        if b_max <= a_max {
            j += W;
        }
    }
    // Scalar two-pointer tail from the current cursors.
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        let bv = *b.get_unchecked(j);
        match av.cmp(&bv) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(av);
                i += 1;
                j += 1;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx2(a, b, out) }
}

/// AVX2 block-vs-block intersection (`W = 4`). AVX2 has no compress, so emit
/// scalar-side.
///
/// While a full 4-lane block of each side remains, load `A = a[i..i+4]` and do
/// an all-pairs compare against the 4-lane `B` block: broadcast each of the four
/// `b` values with `_mm256_set1_epi64x` and `_mm256_cmpeq_epi64` against `A`,
/// OR-reduce the four result vectors into a per-`a`-lane match mask
/// (`_mm256_movemask_pd` over the cast → 4 bits, bit `k` = `a[i+k]` is present in
/// `B`). Emit the matched `a` lanes in lane order (inputs sorted+deduped ⇒ output
/// stays sorted), then advance the side whose block-max is smaller (both on a
/// tie). A scalar two-pointer drains the tail. Bit-identical to the scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    use std::arch::x86_64::*;
    const W: usize = 4;
    let (mut i, mut j) = (0usize, 0usize);
    while i + W <= a.len() && j + W <= b.len() {
        let av = _mm256_loadu_si256(a.as_ptr().add(i) as *const __m256i);
        // All-pairs: OR of cmpeq(A, broadcast(b[j+t])) for t in 0..4.
        let mut eq = _mm256_cmpeq_epi64(av, _mm256_set1_epi64x(*b.get_unchecked(j) as i64));
        eq = _mm256_or_si256(
            eq,
            _mm256_cmpeq_epi64(av, _mm256_set1_epi64x(*b.get_unchecked(j + 1) as i64)),
        );
        eq = _mm256_or_si256(
            eq,
            _mm256_cmpeq_epi64(av, _mm256_set1_epi64x(*b.get_unchecked(j + 2) as i64)),
        );
        eq = _mm256_or_si256(
            eq,
            _mm256_cmpeq_epi64(av, _mm256_set1_epi64x(*b.get_unchecked(j + 3) as i64)),
        );
        // 4-bit per-lane match mask; emit matched `a` lanes in order.
        let mask = _mm256_movemask_pd(_mm256_castsi256_pd(eq)) as u32;
        if mask != 0 {
            for k in 0..W {
                if mask & (1 << k) != 0 {
                    out.push(*a.get_unchecked(i + k));
                }
            }
        }
        let a_max = *a.get_unchecked(i + W - 1);
        let b_max = *b.get_unchecked(j + W - 1);
        if a_max <= b_max {
            i += W;
        }
        if b_max <= a_max {
            j += W;
        }
    }
    // Scalar two-pointer tail from the current cursors.
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        let bv = *b.get_unchecked(j);
        match av.cmp(&bv) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(av);
                i += 1;
                j += 1;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx512_safe(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    unsafe { avx512(a, b, out) }
}

/// AVX-512 block-vs-block intersection (`W = 8`) with hardware compaction.
///
/// While a full 8-lane block of each side remains, load `A = a[i..i+8]` and do an
/// all-pairs compare against the 8-lane `B` block: for each of the eight `b`
/// values, `_mm512_cmpeq_epi64_mask(A, broadcast(b[j+t]))` yields a `__mmask8`;
/// OR the eight masks into a per-`a`-lane match mask. Compact the matched `a`
/// lanes contiguously into `out` with `_mm512_mask_compressstoreu_epi64` (lanes
/// stay in increasing order, so the output stays sorted), then advance the side
/// whose block-max is smaller (both on a tie). A scalar two-pointer drains the
/// tail. Bit-identical to the scalar oracle.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn avx512(a: &[u64], b: &[u64], out: &mut Vec<u64>) {
    use std::arch::x86_64::*;
    const W: usize = 8;
    let (mut i, mut j) = (0usize, 0usize);
    while i + W <= a.len() && j + W <= b.len() {
        let av = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        // All-pairs: OR of cmpeq-mask(A, broadcast(b[j+t])) for t in 0..8.
        let mut mask: __mmask8 = 0;
        for t in 0..W {
            let bcast = _mm512_set1_epi64(*b.get_unchecked(j + t) as i64);
            mask |= _mm512_cmpeq_epi64_mask(av, bcast);
        }
        let cnt = mask.count_ones() as usize;
        if cnt != 0 {
            out.reserve(W);
            let dst = out.as_mut_ptr().add(out.len());
            _mm512_mask_compressstoreu_epi64(dst as *mut i64, mask, av);
            out.set_len(out.len() + cnt);
        }
        let a_max = *a.get_unchecked(i + W - 1);
        let b_max = *b.get_unchecked(j + W - 1);
        if a_max <= b_max {
            i += W;
        }
        if b_max <= a_max {
            j += W;
        }
    }
    // Scalar two-pointer tail from the current cursors.
    while i < a.len() && j < b.len() {
        let av = *a.get_unchecked(i);
        let bv = *b.get_unchecked(j);
        match av.cmp(&bv) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(av);
                i += 1;
                j += 1;
            }
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
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            v.push(Isa::Neon);
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

    #[test]
    fn empty_and_single() {
        check(&[], &[]);
        check(&[42], &[]);
        check(&[], &[42]);
        check(&[42], &[42]); // single, overlapping
        check(&[42], &[7]); // single, disjoint
    }

    #[test]
    fn boundary_values() {
        // 0 and u64::MAX must not break unsigned compares / broadcasts.
        check(&[0], &[0]);
        check(&[u64::MAX], &[u64::MAX]);
        check(&[0, u64::MAX], &[0, u64::MAX]);
        check(&[0, 1, u64::MAX], &[0, u64::MAX]);
        check(&[0, u64::MAX], &[1, u64::MAX - 1]);
        // A longer run anchored at both extremes, spanning multiple blocks.
        let mut a: Vec<u64> = vec![0];
        a.extend(1..40u64);
        a.push(u64::MAX);
        let mut b: Vec<u64> = vec![0];
        b.extend((1..40u64).map(|x| x * 2));
        b.push(u64::MAX);
        check(&a, &b);
    }

    #[test]
    fn no_and_full_overlap_multiblock() {
        // Longer than one SIMD block (>=8) in each lane width, so the wide path
        // and the scalar tail both run. Lengths chosen non-multiples of 8/4/2.
        let evens: Vec<u64> = (0..37u64).map(|x| x * 2).collect();
        let odds: Vec<u64> = (0..37u64).map(|x| x * 2 + 1).collect();
        check(&evens, &odds); // no overlap, multi-block
        check(&evens, &evens); // full overlap, multi-block

        // Partial overlap, both inputs span several blocks, unequal lengths and
        // a non-block-aligned tail on each side.
        let a: Vec<u64> = (0..50u64).collect();
        let b: Vec<u64> = (0..70u64).filter(|x| x % 3 == 0).collect();
        check(&a, &b);
        check(&b, &a);
    }

    #[test]
    fn tail_only_and_block_only() {
        // Shorter than the widest block (8) — exercises the all-scalar-tail path.
        check(&[1, 2, 3], &[2, 3, 4]);
        // Exactly one 8-wide block each, fully consumed by the wide path.
        let a: Vec<u64> = (0..8u64).collect();
        let b: Vec<u64> = (4..12u64).collect();
        check(&a, &b);
    }
}
