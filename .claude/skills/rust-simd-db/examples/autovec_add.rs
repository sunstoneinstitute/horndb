//! Layer 1: let the compiler vectorize a simple columnar arithmetic kernel.
//! No `unsafe`, no per-architecture code — portable everywhere.
//!
//! Build with a modern target to actually get AVX2:
//!   RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --release
//!
//! Verify it vectorized:
//!   cargo asm autovec_add::add_vec     # look for vaddps / vmovups
//!
//! Key idea: zipped iterators (not indexing) remove the per-element bounds
//! check that otherwise blocks the vectorizer.

/// Vectorizable: iterator chain, no indexed access, no panic branch per element.
pub fn add_vec(a: &[f32], b: &[f32], out: &mut [f32]) {
    for ((o, &x), &y) in out.iter_mut().zip(a).zip(b) {
        *o = x + y;
    }
}

/// Also vectorizable: slice to a common length once, then index freely.
pub fn add_sliced(a: &[f32], b: &[f32], out: &mut [f32]) {
    let n = out.len();
    let a = &a[..n];
    let b = &b[..n];
    for i in 0..n {
        out[i] = a[i] + b[i];
    }
}

/// Counter-example: indexed write with mismatched bounds — the per-element
/// bounds check usually defeats the vectorizer. Kept here to contrast.
pub fn add_scalar(a: &[f32], b: &[f32], out: &mut [f32]) {
    for i in 0..out.len() {
        out[i] = a[i] + b[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_agree() {
        for len in [0usize, 1, 7, 8, 33, 257] {
            let a: Vec<f32> = (0..len).map(|i| i as f32 * 1.5).collect();
            let b: Vec<f32> = (0..len).map(|i| i as f32 - 2.0).collect();
            let (mut v, mut s, mut z) = (vec![0.0; len], vec![0.0; len], vec![0.0; len]);
            add_vec(&a, &b, &mut v);
            add_sliced(&a, &b, &mut s);
            add_scalar(&a, &b, &mut z);
            assert_eq!(v, s);
            assert_eq!(v, z);
        }
    }
}
