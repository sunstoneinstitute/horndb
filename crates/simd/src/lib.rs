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
//! ## Kernel selection: known-CPU table → calibration (`HORNDB_SIMD_AUTOTUNE`)
//!
//! The fastest kernel is **workload- and host-dependent**, and real LDBC SPB-256
//! measurements proved the SIMD kernels are net-*harmful* versus scalar on the
//! CPUs we run on (both AMD Zen4 and Intel Sapphire Rapids): the balanced,
//! L2-resident calibration inputs don't match the skewed, memory-bound access
//! patterns production dispatches. The AVX2/NEON `lower_bound` (gallop + linear
//! window scan) is the dominant culprit — it loses to scalar `partition_point`
//! binary search on the seek-heavy leapfrog path. So each primitive resolves its
//! kernel through this priority:
//!
//! 1. [`with_forced_isa`] — test/bench override; bypasses everything below.
//! 2. `HORNDB_SIMD_MAX_ISA` cap — bounds the candidate set for all lower tiers.
//! 3. **Known-CPU table** (`cpu.rs`) — keyed on CPUID vendor/family/model and
//!    populated from real SPB-256 measurements. A table hit selects a kernel with
//!    **no timing**. Today both known rows (AMD Zen4 Ryzen 7 7700, Intel Xeon
//!    Gold 5412U / Sapphire Rapids) pin every kernel to scalar.
//! 4. **Representative-input calibration** (`HORNDB_SIMD_AUTOTUNE`, default on):
//!    for an unlisted CPU, time every cap-allowed kernel on inputs shaped like the
//!    production access pattern (seek-sweep for `lower_bound`, >L2 base for
//!    `gather`, moderate selectivity for `filter_indices_eq`) and adopt a SIMD
//!    kernel only when it beats scalar by a safety margin.
//! 5. Static widest-ISA preference (autotune off) / scalar baseline.
//!
//! Disable calibration with `HORNDB_SIMD_AUTOTUNE=off` (also `0`/`false`/`no`),
//! which falls back to the static widest-ISA preference; the `HORNDB_SIMD_MAX_ISA`
//! cap still bounds the candidate set in every mode. Call [`init`] at startup to
//! pay the (small) calibration cost up front; [`calibration_report`] returns the
//! chosen `(kernel, ISA, source)` per primitive for startup logging, where the
//! [`Source`] records which tier chose it (table / calibrated / static). The same
//! source is exported on the `horndb_simd_kernel_isa` metric's `source` label.
//! [`configured_autotune`] reports whether auto-tune is enabled.

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
pub use dispatch::{configured_autotune, configured_max_isa, forced_isa, Isa, Source};

/// The host's human-readable CPU identity for startup logging — the CPU brand
/// string where the arch exposes one (x86_64 via CPUID, Apple Silicon via
/// sysctl), else a `"<vendor> family <f> model <m>"` fallback, or `None` on an
/// unidentifiable arch (e.g. aarch64 Linux). Independent of the kernel table:
/// a host with no table row still reports which CPU it is, so a `source=calibrated`
/// selection can be tied back to the hardware that produced it. Pairs with
/// [`calibration_report`], [`configured_max_isa`], and [`configured_autotune`].
pub fn cpu_identity() -> Option<String> {
    cpu::identity()
}

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

/// The kernel chosen for each dispatched primitive, one `(kernel, isa, source)`
/// per primitive: the [`Kernel`], the [`Isa`] its calibration picked, and the
/// [`Source`] selection path that chose it. Triggers calibration for any
/// not-yet-resolved primitive. Intended for startup logging, e.g.
/// `for (k, isa, src) in horndb_simd::calibration_report() { tracing::info!(name = k.name(), ?isa, source = src.name()); }`.
pub fn calibration_report() -> Vec<(Kernel, Isa, Source)> {
    // Pair each `Kernel` variant with its primitive's `chosen()`/`source()`.
    // Order is the stable report order (intersect, lower_bound, …, gather).
    type ReportEntry = (Kernel, fn() -> Isa, fn() -> Source);
    let entries: [ReportEntry; 7] = [
        (Kernel::Intersect, intersect::chosen, intersect::source),
        (Kernel::LowerBound, lower_bound::chosen, lower_bound::source),
        (Kernel::Merge, merge::chosen, merge::source),
        (Kernel::Dedup, dedup::chosen, dedup::source),
        (Kernel::FilterRange, filter::chosen, filter::source),
        (
            Kernel::FilterIndicesEq,
            filter_indices::chosen,
            filter_indices::source,
        ),
        (Kernel::Gather, gather::chosen, gather::source),
    ];
    entries
        .into_iter()
        .map(|(k, chosen, source)| (k, chosen(), source()))
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
        let report = calibration_report();
        let names: Vec<&'static str> = report.iter().map(|(k, _, _)| k.name()).collect();
        assert_eq!(names, expected);
        // Every entry carries a selection source (one of table/calibrated/static).
        for (k, _, source) in &report {
            assert!(
                matches!(source, Source::Table | Source::Calibrated | Source::Static),
                "{}: unexpected source {source:?}",
                k.name()
            );
        }
    }
}
