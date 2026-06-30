//! Benchmark scalar vs autovectorized vs intrinsics on the SAME machine.
//! A "3x speedup" is meaningless without the baseline and the build flags.
//!
//! Cargo.toml:
//!   [dev-dependencies]
//!   divan = "0.1"
//!   [[bench]]
//!   name = "bench_divan"
//!   harness = false
//!
//! Run (set the target so AVX2 is actually available):
//!   RUSTFLAGS="-C target-cpu=x86-64-v3" cargo bench --bench bench_divan
//!
//! divan is chosen here for terse parametric benches; criterion is the
//! heavier alternative with statistical regression detection.

fn main() {
    divan::main();
}

fn inputs(len: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let a: Vec<f32> = (0..len).map(|i| i as f32 * 1.5).collect();
    let b: Vec<f32> = (0..len).map(|i| i as f32 - 2.0).collect();
    (a, b, vec![0.0; len])
}

// Vary the length so you can see vectorization pay off as it amortizes.
const LENS: &[usize] = &[64, 1024, 65536];

#[divan::bench(args = LENS)]
fn scalar_indexed(bencher: divan::Bencher, len: usize) {
    let (a, b, mut out) = inputs(len);
    bencher.bench_local(|| {
        // indexed: bounds check per element, usually not vectorized
        for i in 0..out.len() {
            out[i] = a[i] + b[i];
        }
        divan::black_box(&out);
    });
}

#[divan::bench(args = LENS)]
fn autovec_zip(bencher: divan::Bencher, len: usize) {
    let (a, b, mut out) = inputs(len);
    bencher.bench_local(|| {
        for ((o, &x), &y) in out.iter_mut().zip(&a).zip(&b) {
            *o = x + y;
        }
        divan::black_box(&out);
    });
}

// Compare the two and confirm autovec_zip wins (and by roughly the vector
// width) once len is large enough to amortize loop setup. If it does NOT win,
// the loop didn't vectorize — check target-cpu and the bounds-check blockers
// before reaching for intrinsics.
