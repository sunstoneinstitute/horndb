# Layers 2 & 3 — Portable SIMD, intrinsics, and runtime dispatch

When autovectorization can't reach the kernel, write explicit SIMD. The portability/effort tradeoff:

| Option                     | Portable? | Stable? | Effort | Use when                                        |
|----------------------------|-----------|---------|--------|-------------------------------------------------|
| `std::simd` (`Simd<T,N>`)  | yes (all LLVM targets) | **no — nightly #86656** | low | nightly OK; want one code path everywhere |
| `wide` / `pulp` / `macerator` crates | yes | yes | low–med | stable + portable vector logic |
| `core::arch` intrinsics    | no (per-arch) | yes | high | stable + need a specific instruction (gather, shuffle, AVX-512) |

`std::simd` is the cross-platform ideal (compiles for every target, no per-arch paths, supports every instruction set LLVM does) but is **nightly-only as of 2026** — re-check the tracking issue before asserting this. Stable-pinned projects (1.90+) must use `core::arch` or a crate.

## The canonical dispatch pattern (production-proven)

Runtime feature detection → `#[target_feature]`-gated impl → scalar fallback. This is what Vortex's `take` does (AVX2 gather vs `take_primitive_scalar`).

```rust
pub fn add_dispatch(a: &[f32], b: &[f32], out: &mut [f32]) {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: guarded by the runtime check above.
            return unsafe { add_avx2(a, b, out) };
        }
    }
    add_scalar(a, b, out); // always-correct fallback
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn add_avx2(a: &[f32], b: &[f32], out: &mut [f32]) {
    use std::arch::x86_64::*;
    // ... _mm256_loadu_ps / _mm256_add_ps / _mm256_storeu_ps ...
}

fn add_scalar(a: &[f32], b: &[f32], out: &mut [f32]) {
    for ((o, &x), &y) in out.iter_mut().zip(a).zip(b) { *o = x + y; }
}
```

### Rules that keep this correct and fast

- **Detect once, not per call.** `is_x86_feature_detected!` codegen is slower than a plain branch. Resolve the best implementation at startup and pass a *token* (a fn pointer or capability struct) down through the call tree. See the `OnceLock` pattern below.
- **`target_feature 1.1` (stable since Rust 1.86)** lets a `#[target_feature(enable="avx2")]` fn call another fn with the same or a superset gate **without `unsafe`** — eliminating most boilerplate inside a SIMD module. You still need `unsafe` + a runtime check at the *boundary* where an ungated fn calls a gated one.
- **x86 needs dispatch; aarch64 NEON does not.** NEON/Advanced SIMD is mandatory on every ARMv8-A 64-bit CPU, so it's always available — gate x86 paths, but NEON can be unconditional under `#[cfg(target_arch = "aarch64")]`.
- **Never call a gated intrinsic without confirming support.** Executing an unsupported instruction is UB (SIGILL). This is why all `core::arch` intrinsics are `unsafe`.

## Resolve-once with `OnceLock`

```rust
use std::sync::OnceLock;

type Kernel = fn(&[f32], &[f32], &mut [f32]);

fn best_add() -> Kernel {
    static CELL: OnceLock<Kernel> = OnceLock::new();
    *CELL.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            return |a, b, o| unsafe { add_avx2(a, b, o) };
        }
        add_scalar
    })
}
```

## Multiversioning crates (skip hand-rolling)

- **`multiversion`** — proc-macro `#[multiversion(targets("x86_64+avx2", ...))]` compiles N feature-specific copies of a whole function and dispatches at runtime; ships a large CPU-model database. Best when you want "make this whole function fast on whatever CPU."
- **`target-feature-dispatch`** — declarative macro usable in expression position; no proc-macro, no CPU-model DB, tracks the latest compiler, and caches the resolved function pointer via `OnceLock` so dispatch resolves only once. Best when you want lighter deps and explicit control.

Both encapsulate the detect-once + fallback discipline above.
