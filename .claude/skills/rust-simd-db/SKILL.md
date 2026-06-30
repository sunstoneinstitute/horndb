---
name: rust-simd-db
description: This skill should be used when writing, reviewing, or optimizing SIMD-vectorized database/columnar kernels in Rust — "vectorize this kernel", "SIMD in Rust", "autovectorization", "std::simd", "core::arch intrinsics", "target_feature", "multiversion", "AVX2/AVX-512/NEON", "columnar scan/filter/take", "bitpacking decode", "avoid gather/scatter", or "scalar fallback parity test". Covers the layered strategy (autovec → portable SIMD → intrinsics), runtime dispatch, maintainability, and memory layout for arrow-rs/datafusion/polars/vortex-style code.
---

# Rust SIMD for Database Kernels

Performance tricks and maintenance practices for vectorizing columnar/database operations in Rust (1.90+). The guidance is evidence-based; see [references/sources.md](references/sources.md) for citations and the confidence of each claim.

## Core principle: a layered strategy, not "always hand-write SIMD"

Pick the **lowest-effort layer that gets the speedup**, and only descend when measurement justifies it:

```
1. Autovectorization  → simple elementwise kernels (arithmetic, filter, SoA maps)
2. Portable SIMD       → cross-platform vector logic   (std::simd nightly, or `wide`/`pulp` on stable)
3. core::arch intrinsics → irregular kernels autovec can't reach (gather-take, bitpack/FastLanes decode, dict decode)
```

**Do not generalize "autovec always beats intrinsics."** That holds for *simple elementwise* kernels (arrow-rs deleted its hand-written SIMD arithmetic because autovec was ~23–46% faster). For **irregular** DB kernels — gather-based `take`, bitpacking, dictionary/RLE decode, predicate-to-bitmap — hand-written intrinsics still win, which is exactly why Vortex and arrow keep them. Measure, don't assume.

## Decision checklist

- [ ] **Is the kernel a simple elementwise/SoA loop?** Try autovectorization first (layer 1). Verify it actually vectorized (`cargo asm`, `-C target-cpu=...`, or a benchmark) — autovec is silent when it fails.
- [ ] **Did autovec fail or is the access pattern irregular?** Move to portable SIMD or intrinsics.
- [ ] **Need stable Rust?** `std::simd` is **nightly-only** (tracking issue #86656, still open as of 2026 — re-check current status). On stable, use `core::arch` intrinsics or a crate (`wide`, `pulp`, `macerator`).
- [ ] **Using x86 AVX/AVX2/AVX-512?** Requires runtime CPU detection + dispatch (x86-64 baseline is only SSE2). aarch64 NEON is mandatory on all ARMv8-A — no gate needed.
- [ ] **Wrote `unsafe` intrinsics?** Add a scalar fallback and a parity test (SIMD output must equal scalar output).

## Layer 1 — Autovectorization (do this first)

The #1 blocker is **per-element slice bounds checking**: the compiler inserts a panic branch on every indexed write that the vectorizer cannot reorder. Unlock autovec **without `unsafe`**:

- Iterate (`for (o, a) in out.iter_mut().zip(a)`) instead of indexing `out[i] = a[i] + b[i]`.
- Slice all inputs to the same length **before** the loop (`let a = &a[..n];`) so the bounds check is hoisted once.
- Use struct-of-arrays (SoA) types, not array-of-structs — SoA both enables autovec and avoids gather/scatter.
- Compile with `-C target-cpu=x86-64-v3` (AVX2) or similar; the default x86-64 target only emits SSE2. **Avoid `target-cpu=native` for distributed binaries** — it may emit AVX-512 absent on the user's CPU and crash (SIGILL).

Note: stable rustc **does** autovectorize float loops (subject to reassociation/fast-math limits). The claim that it won't was refuted.

See [references/autovectorization.md](references/autovectorization.md) and [examples/autovec_add.rs](examples/autovec_add.rs).

## Layer 2/3 — Portable SIMD and intrinsics with dispatch

The canonical maintainable pattern: **runtime feature detection → `#[target_feature]`-gated impl → scalar fallback.** Vortex's `take` does exactly this (`is_x86_feature_detected!("avx2")` → AVX2 gather kernel, else `take_primitive_scalar`).

- Detect features **once at startup**, pass the result down as a token — don't re-detect per call (per-call detection codegen is slower).
- Use `target_feature 1.1` (stable since Rust 1.86): a `#[target_feature]` fn can safely call another with the same/superset gate, killing most `unsafe` boilerplate. Calling a gated fn from an *ungated* fn still needs `unsafe` + detection.
- For ergonomic multiversioning without hand-rolling dispatch: `multiversion` (proc-macro, large CPU-model DB) or `target-feature-dispatch` (declarative, caches the resolved fn pointer via `OnceLock`).

See [references/dispatch-and-multiversioning.md](references/dispatch-and-multiversioning.md) and [examples/dispatch_take.rs](examples/dispatch_take.rs).

## Maintainability & memory layout

- **Scalar fallback is mandatory** and is also your test oracle — fuzz/property-test SIMD output against it across feature levels.
- **UB rules:** `Simd<T,N>` has greater alignment than `[T;N]`. When reading/writing `Simd` through pointers from a `T`-aligned slice, use `read_unaligned`/`write_unaligned`; the aligned `read`/`write` on misaligned data is UB. Executing an intrinsic on an unsupported CPU is UB — the reason all `core::arch` intrinsics are `unsafe`.
- **Alignment & gather:** unaligned/SoA layout choices can swing perf 10–20%. Prefer contiguous LOAD over gather/scatter where possible. SIMD-friendly encodings like **FastLanes** bitpacking (used by Vortex) are laid out so decode auto-vectorizes by bit-width alone.

See [references/maintainability-and-layout.md](references/maintainability-and-layout.md).

## Database kernels reference

Concrete techniques per kernel (scan/filter, take/gather, bitpacking, dictionary/varint decode, string compare, hash/aggregation) and which crates use them: [references/db-kernels.md](references/db-kernels.md).
