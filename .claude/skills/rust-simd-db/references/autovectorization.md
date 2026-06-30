# Layer 1 — Autovectorization

The compiler (LLVM) will vectorize simple loops on its own — no `unsafe`, no per-arch code, portable everywhere. This is the **default** for simple elementwise/SoA kernels. arrow-rs deleted its hand-written SIMD arithmetic kernels precisely because autovec was faster (~23–46%).

## Why autovec silently fails, and how to fix it

### 1. Per-element bounds checks (the #1 blocker)

`out[i] = a[i] + b[i]` makes the compiler insert a panic branch on every indexed write. That branch is a side-effecting control-flow edge the vectorizer can't reorder, so the loop stays scalar.

**Fix — iterate instead of index:**

```rust
// BAD: indexed — bounds check per element, usually NOT vectorized
fn add_scalar(a: &[f32], b: &[f32], out: &mut [f32]) {
    for i in 0..out.len() {
        out[i] = a[i] + b[i];
    }
}

// GOOD: zipped iterators — no per-element bounds check, vectorizes
fn add_vec(a: &[f32], b: &[f32], out: &mut [f32]) {
    for ((o, &x), &y) in out.iter_mut().zip(a).zip(b) {
        *o = x + y;
    }
}
```

**Fix — slice to a common length first (hoist the check once):**

```rust
fn add_sliced(a: &[f32], b: &[f32], out: &mut [f32]) {
    let n = out.len();
    let (a, b) = (&a[..n], &b[..n]); // one bounds check, then indexing is free
    for i in 0..n {
        out[i] = a[i] + b[i];
    }
}
```

You can also hoist `assert!(a.len() >= n)` before the loop; the optimizer uses the assertion to elide later checks.

### 2. Default target is too old

The x86-64 ABI baseline is **SSE2 only**. Without flags, the compiler won't emit AVX/AVX2. Set a microarchitecture level:

```toml
# .cargo/config.toml — choose ONE strategy
[build]
rustflags = ["-C", "target-cpu=x86-64-v3"] # AVX2+FMA, safe on ~2015+ CPUs
```

| Level        | Implies            | Notes                                  |
|--------------|--------------------|----------------------------------------|
| `x86-64`     | SSE2               | default; weakest                       |
| `x86-64-v2`  | SSE4.2             | ~2009+                                 |
| `x86-64-v3`  | AVX2, FMA, BMI     | good default for modern servers        |
| `x86-64-v4`  | AVX-512            | newest only; risky to ship             |

**Do NOT ship binaries built with `target-cpu=native`** — it bakes in whatever the build host had (possibly AVX-512) and SIGILLs on older targets. Use a microarch level, or runtime dispatch (see dispatch-and-multiversioning.md) when you must support a range.

### 3. Array-of-structs layout

Interleaved fields force the compiler toward gather/scatter. Use **struct-of-arrays**:

```rust
// AoS — fights the vectorizer
struct Row { x: f32, y: f32 }
let rows: Vec<Row>;

// SoA — vectorizes, contiguous loads
struct Cols { x: Vec<f32>, y: Vec<f32> }
```

Columnar formats (Arrow, Vortex) are SoA by construction — that's a major reason their scan/filter kernels vectorize well.

## Verify it actually vectorized

Autovec is silent. Confirm before claiming a speedup:

```bash
# Inspect emitted asm for vector instructions (vaddps, vmovups, ...)
cargo install cargo-show-asm
cargo asm --rust your_crate::add_vec

# Or benchmark scalar vs vectorized variants (see examples/bench_divan.rs)
```

If you don't see `v...ps`/`v...d` (AVX) or `addps` (SSE) instructions and a width-proportional speedup, it didn't vectorize — fix the blockers above before reaching for intrinsics.

## When autovec is the wrong tool

Autovec handles *regular, elementwise* work. It will not produce: gather-based `take`, bit-unpacking across word boundaries, dictionary/RLE decode, or predicate→bitmap packing. Those need explicit SIMD (layers 2/3).
