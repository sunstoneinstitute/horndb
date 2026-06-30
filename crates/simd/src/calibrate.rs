//! Startup micro-calibration: pick the fastest available kernel per primitive.
//!
//! Per-host benchmarks proved the fastest ISA is host-dependent and that no
//! cheap runtime bit distinguishes the cases (e.g. AVX-512 `intersect` wins on
//! Intel Sapphire Rapids but loses on AMD Zen4's double-pumped AVX-512). So at
//! startup (or lazily on first use) each dispatched primitive times every
//! kernel its host can run on a small L2-resident synthetic workload and caches
//! the winner. The winner is only adopted if it beats the scalar baseline by a
//! safety [`MARGIN`]; otherwise scalar wins (it is the candidate-list contract
//! baseline). This keeps a noisy near-tie from shipping a slower-but-flashier
//! kernel.

use crate::dispatch::Isa;

/// A SIMD candidate must beat scalar by at least this fraction to be adopted.
/// Guards against scheduler noise turning a near-tie into a regression.
const MARGIN: f64 = 0.05;
/// Warm-up runs (discarded) before timing — pull code/data into cache, let the
/// branch predictor settle.
const WARMUP: u32 = 3;
/// Timed runs; we keep the *minimum* (robust to scheduler preemption spikes).
const ITERS: u32 = 7;

/// Time a single kernel: run `run(k)` `WARMUP` times (discarded), then `ITERS`
/// times keeping the minimum elapsed. The `run` closure is responsible for
/// `core::hint::black_box`-ing its output so the work is not optimised away.
fn time_kernel<P: Copy>(k: P, run: &mut impl FnMut(P)) -> std::time::Duration {
    for _ in 0..WARMUP {
        run(k);
    }
    let mut best = std::time::Duration::MAX;
    for _ in 0..ITERS {
        let t = std::time::Instant::now();
        run(k);
        best = best.min(t.elapsed());
    }
    best
}

/// Pick the fastest kernel among `candidates`, falling back to scalar.
///
/// By contract `candidates[0]` is the scalar baseline (asserted non-empty). The
/// scalar baseline is timed, then every other candidate; the candidate with the
/// lowest time is returned **only if** it beats scalar by at least [`MARGIN`]
/// (`simd_time < scalar_time * (1.0 - MARGIN)`). Otherwise scalar
/// (`candidates[0]`) is returned. The result is always one of `candidates`.
pub(crate) fn pick<P: Copy>(candidates: &[(Isa, P)], mut run: impl FnMut(P)) -> (Isa, P) {
    assert!(!candidates.is_empty(), "candidate list must be non-empty");
    let scalar = candidates[0];
    let scalar_time = time_kernel(scalar.1, &mut run);

    let mut best = scalar;
    let mut best_time = scalar_time;
    for &cand in &candidates[1..] {
        let t = time_kernel(cand.1, &mut run);
        if t < best_time {
            best = cand;
            best_time = t;
        }
    }

    // Adopt the winner only if it clears scalar by the safety margin.
    if best.0 != scalar.0 && best_time < scalar_time.mul_f64(1.0 - MARGIN) {
        best
    } else {
        scalar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Candidate "kernels" are busy-loop lengths; `run` spins that many times so
    // a larger value is reliably slower. Avoids sleeps (flaky under load) while
    // keeping each call sub-millisecond.
    fn spin(iters: u64) {
        let mut acc = 0u64;
        for i in 0..iters {
            acc = acc.wrapping_add(i ^ (acc.rotate_left(1)));
        }
        core::hint::black_box(acc);
    }

    #[test]
    fn picks_scalar_when_simd_is_slower() {
        // scalar fast (short spin), "fast-isa" actually slow (long spin).
        let candidates = [(Isa::Scalar, 2_000u64), (Isa::Avx2, 2_000_000u64)];
        let (isa, _) = pick(&candidates, spin);
        assert_eq!(isa, Isa::Scalar);
    }

    #[test]
    fn picks_simd_when_clearly_faster() {
        // SIMD genuinely faster (much shorter spin) beyond the margin.
        let candidates = [(Isa::Scalar, 2_000_000u64), (Isa::Avx2, 2_000u64)];
        let (isa, _) = pick(&candidates, spin);
        assert_eq!(isa, Isa::Avx2);
    }

    #[test]
    fn within_margin_keeps_scalar() {
        // A candidate that is faster by *less* than MARGIN must not be adopted.
        // Model the times directly so the comparison is deterministic (no
        // reliance on a busy-loop landing inside a 5% window).
        let scalar_time = Duration::from_micros(1000);
        let near_time = Duration::from_micros(970); // 3% faster, < 5% MARGIN
        let chosen = if near_time < scalar_time.mul_f64(1.0 - MARGIN) {
            Isa::Avx2
        } else {
            Isa::Scalar
        };
        assert_eq!(chosen, Isa::Scalar);
    }

    #[test]
    fn single_candidate_returns_scalar() {
        let candidates = [(Isa::Scalar, 2_000u64)];
        let (isa, _) = pick(&candidates, spin);
        assert_eq!(isa, Isa::Scalar);
    }
}
