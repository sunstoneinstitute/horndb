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

pub use dispatch::{configured_max_isa, forced_isa, Isa};

/// Test-support API (see [`dispatch::with_forced_isa`]): pins a specific ISA
/// dispatch path for the differential proptests and the intersect bench, which
/// compile this crate as an ordinary dependency. Production callers never use
/// it.
pub use dispatch::with_forced_isa;
