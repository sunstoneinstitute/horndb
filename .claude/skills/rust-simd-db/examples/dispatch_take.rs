//! Layers 2/3: an irregular kernel (`take` by arbitrary indices = gather) that
//! autovectorization cannot reach. Demonstrates the canonical maintainable
//! pattern used by Vortex: runtime feature detection -> #[target_feature] AVX2
//! gather kernel -> scalar fallback, plus a resolve-once dispatcher.
//!
//! Stable Rust 1.90+. No nightly std::simd needed.

/// Public entry point. Picks the best implementation once and caches it.
pub fn take_i32(values: &[i32], indices: &[u32], out: &mut [i32]) {
    best_take()(values, indices, out)
}

type TakeFn = fn(&[i32], &[u32], &mut [i32]);

/// Resolve the implementation ONCE (not per call) and cache the fn pointer.
fn best_take() -> TakeFn {
    use std::sync::OnceLock;
    static CELL: OnceLock<TakeFn> = OnceLock::new();
    *CELL.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            return |v, i, o| unsafe { take_avx2(v, i, o) };
        }
        take_scalar
    })
}

/// Always-correct fallback. Also the test oracle for the SIMD path.
pub fn take_scalar(values: &[i32], indices: &[u32], out: &mut [i32]) {
    for (o, &idx) in out.iter_mut().zip(indices) {
        *o = values[idx as usize];
    }
}

/// AVX2 gather kernel. 8 lanes of i32 per iteration via _mm256_i32gather_epi32.
///
/// SAFETY: only call when `is_x86_feature_detected!("avx2")` is true (enforced
/// by the dispatcher above). Indices must be in-bounds for `values`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn take_avx2(values: &[i32], indices: &[u32], out: &mut [i32]) {
    use std::arch::x86_64::*;

    let base = values.as_ptr();
    let n = out.len().min(indices.len());
    let chunks = n / 8;

    for c in 0..chunks {
        let off = c * 8;
        // SAFETY: offsets stay within `indices`/`out`; intrinsics are enabled
        // by the #[target_feature(enable = "avx2")] gate (Rust edition 2024
        // requires explicit `unsafe` blocks even inside a target-feature fn).
        unsafe {
            // Load 8 indices.
            let idx = _mm256_loadu_si256(indices.as_ptr().add(off) as *const __m256i);
            // Gather 8 i32 from base[idx[k]]; scale = 4 bytes per i32.
            let gathered = _mm256_i32gather_epi32::<4>(base, idx);
            _mm256_storeu_si256(out.as_mut_ptr().add(off) as *mut __m256i, gathered);
        }
    }

    // Remainder (tail) — the classic place SIMD bugs hide. Handle with scalar.
    // SAFETY: k < n <= out.len()/indices.len(); index values are caller-promised in-bounds.
    for k in (chunks * 8)..n {
        unsafe {
            *out.get_unchecked_mut(k) = *values.get_unchecked(*indices.get_unchecked(k) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simd_matches_scalar() {
        let values: Vec<i32> = (0..1000).collect();
        for len in [0usize, 1, 7, 8, 9, 16, 17, 255] {
            let indices: Vec<u32> = (0..len).map(|i| ((i * 37) % 1000) as u32).collect();
            let mut a = vec![0; len];
            let mut b = vec![0; len];
            take_scalar(&values, &indices, &mut a);
            take_i32(&values, &indices, &mut b);
            assert_eq!(a, b, "mismatch at len={len}");
        }
    }
}
