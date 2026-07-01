//! `horndb-simd` — runtime-dispatched SIMD primitives over primitive slices.
//!
//! SPEC-12. A dependency-free leaf crate: every primitive is a safe wrapper
//! that dispatches once to a scalar / AVX2 / AVX-512 / NEON kernel and is
//! proven bit-identical to the scalar oracle by a differential proptest.
//! This crate is the *only* place in the workspace allowed to carry
//! hand-written SIMD intrinsics.
//!
//! ## Dispatch and the `HORNDB_SIMD_MAX_ISA` cap
//!
//! Each primitive resolves the widest kernel the CPU supports (via
//! `is_x86_feature_detected!` / `is_aarch64_feature_detected!`) **once**, caching
//! a function pointer. The binary never raises its compile-time target-feature
//! baseline, so it runs on any x86-64 / aarch64 host and simply picks a narrower
//! kernel where a feature is absent.
//!
//! Set the environment variable `HORNDB_SIMD_MAX_ISA` to cap the selection
//! without a rebuild — an operational knob, read once at first use:
//!
//! - `HORNDB_SIMD_MAX_ISA=avx2` — disable AVX-512 fleet-wide (e.g. if Zen4
//!   AVX-512 downclocking loses net on your workload).
//! - `HORNDB_SIMD_MAX_ISA=scalar` — disable all SIMD (escape hatch for isolating
//!   a suspected kernel regression in production).
//!
//! The cap is a width *tier* (`scalar` < `avx2` ≈ `neon` < `avx512`). It affects
//! only production detection, not the test/bench [`with_forced_isa`] override, so
//! the differential suite still exercises every kernel the host can run.
//! Query the active cap with [`configured_max_isa`].
//!
//! ## Startup calibration (`HORNDB_SIMD_AUTOTUNE`)
//!
//! Benchmarks proved the fastest ISA is **host-dependent** with no cheap runtime
//! bit to tell hosts apart: AVX-512 `intersect` wins ~2.5× on Intel Sapphire
//! Rapids but *loses* ~2.5× on AMD Zen4's double-pumped AVX-512; SIMD
//! `lower_bound` loses to scalar binary search on both. So each primitive
//! **micro-calibrates** on first use (or eagerly via [`init`]): it times every
//! kernel its host can run on a small L2-resident workload and caches the
//! fastest, adopting a SIMD kernel only when it beats scalar by a safety margin.
//!
//! Calibration is **on by default**. Disable it with `HORNDB_SIMD_AUTOTUNE=off`
//! (also `0`/`false`/`no`), which falls back to the static widest-ISA
//! preference. The `HORNDB_SIMD_MAX_ISA` cap still bounds the candidate set in
//! both modes. Call [`init`] at startup to pay the calibration cost up front;
//! [`calibration_report`] returns the chosen ISA per primitive (for logging),
//! and [`configured_autotune`] reports whether auto-tune is enabled.

mod calibrate;
mod cpu;
mod dedup;
mod dispatch;
mod filter;
mod filter_indices;
mod gather;
mod intersect;
mod lower_bound;
mod merge;
mod scalar;

pub use dedup::dedup;
pub use filter::{filter, filter_range};
pub use filter_indices::filter_indices_eq;
pub use gather::gather;
pub use intersect::intersect;
pub use lower_bound::lower_bound;
pub use merge::merge;

pub use cpu::Kernel;
pub use dispatch::{configured_autotune, configured_max_isa, forced_isa, Isa};

/// Eagerly calibrate every primitive's kernel, paying the (small) startup
/// calibration cost up front instead of lazily on first use. Hosts that want
/// deterministic first-call latency call this once at startup; otherwise each
/// primitive calibrates on its first call. A no-op beyond the first call per
/// primitive (results are cached). Honours `HORNDB_SIMD_AUTOTUNE` and
/// `HORNDB_SIMD_MAX_ISA`.
pub fn init() {
    intersect::prime();
    lower_bound::prime();
    merge::prime();
    dedup::prime();
    filter::prime();
    filter_indices::prime();
    gather::prime();
}

/// The kernel ISA chosen for each dispatched primitive, one `(name, isa)` per
/// primitive. Triggers calibration for any not-yet-resolved primitive. Intended
/// for startup logging, e.g.
/// `for (n, isa) in horndb_simd::calibration_report() { tracing::info!(%n, ?isa); }`.
pub fn calibration_report() -> Vec<(&'static str, Isa)> {
    // Pair each `Kernel` variant with its primitive's `chosen()`, deriving the
    // name from `Kernel::name()` so it can never drift from the enum. Order is
    // the stable report order (intersect, lower_bound, …, gather).
    let entries: [(Kernel, fn() -> Isa); 7] = [
        (Kernel::Intersect, intersect::chosen),
        (Kernel::LowerBound, lower_bound::chosen),
        (Kernel::Merge, merge::chosen),
        (Kernel::Dedup, dedup::chosen),
        (Kernel::FilterRange, filter::chosen),
        (Kernel::FilterIndicesEq, filter_indices::chosen),
        (Kernel::Gather, gather::chosen),
    ];
    entries
        .into_iter()
        .map(|(k, chosen)| (k.name(), chosen()))
        .collect()
}

/// Test-support API (see [`dispatch::with_forced_isa`]): pins a specific ISA
/// dispatch path for the differential proptests and the intersect bench, which
/// compile this crate as an ordinary dependency. Production callers never use
/// it.
pub use dispatch::with_forced_isa;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_report_names_match_kernel_names() {
        // The report must expose exactly each `Kernel`'s `name()`, in the stable
        // order — so the two lists can never drift.
        let expected = [
            Kernel::Intersect.name(),
            Kernel::LowerBound.name(),
            Kernel::Merge.name(),
            Kernel::Dedup.name(),
            Kernel::FilterRange.name(),
            Kernel::FilterIndicesEq.name(),
            Kernel::Gather.name(),
        ];
        let names: Vec<&'static str> = calibration_report().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, expected);
    }
}
