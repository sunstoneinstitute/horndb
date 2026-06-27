//! `horndb-simd` — runtime-dispatched SIMD primitives over primitive slices.
//!
//! SPEC-12. A dependency-free leaf crate: every primitive is a safe wrapper
//! that dispatches once to a scalar / AVX2 / AVX-512 / NEON kernel and is
//! proven bit-identical to the scalar oracle by a differential proptest.
//! This crate is the *only* place in the workspace allowed to carry
//! hand-written SIMD intrinsics.

mod dispatch;
mod lower_bound;
mod scalar;

pub use lower_bound::lower_bound;

pub use dispatch::{forced_isa, Isa};

#[cfg(test)]
pub use dispatch::with_forced_isa;
