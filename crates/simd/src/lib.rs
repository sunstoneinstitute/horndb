//! `horndb-simd` — runtime-dispatched SIMD primitives over primitive slices.
//!
//! SPEC-12. A dependency-free leaf crate: every primitive is a safe wrapper
//! that dispatches once to a scalar / AVX2 / AVX-512 / NEON kernel and is
//! proven bit-identical to the scalar oracle by a differential proptest.
//! This crate is the *only* place in the workspace allowed to carry
//! hand-written SIMD intrinsics.

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

pub use dispatch::{forced_isa, Isa};

/// Test-support API (see [`dispatch::with_forced_isa`]): pins a specific ISA
/// dispatch path for the differential proptests and the intersect bench, which
/// compile this crate as an ordinary dependency. Production callers never use
/// it.
pub use dispatch::with_forced_isa;
