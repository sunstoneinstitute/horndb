# Maintainability, UB avoidance, and memory layout

SIMD code rots fast and breaks silently. These practices keep it correct, testable, and portable.

## Scalar fallback is mandatory — and it's your oracle

Every SIMD kernel needs a scalar twin, for two reasons:

1. **Correctness on unsupported CPUs** — the fallback runs when the feature is absent.
2. **It is the test oracle** — the SIMD path is correct iff its output equals the scalar path's, for all inputs.

### Parity testing

```rust
#[test]
fn avx2_matches_scalar() {
    if !is_x86_feature_detected!("avx2") { return; } // skip on CI without AVX2
    for len in [0usize, 1, 7, 8, 9, 31, 32, 33, 1000] { // hit tail/remainder cases
        let a: Vec<f32> = (0..len).map(|i| i as f32 * 1.5).collect();
        let b: Vec<f32> = (0..len).map(|i| i as f32 - 3.0).collect();
        let mut s = vec![0.0; len];
        let mut v = vec![0.0; len];
        add_scalar(&a, &b, &mut s);
        unsafe { add_avx2(&a, &b, &mut v) };
        assert_eq!(s, v, "mismatch at len={len}");
    }
}
```

- **Property-test** with `proptest`/`quickcheck` for random lengths and values — SIMD bugs hide in the **remainder/tail** (lengths not a multiple of the vector width) and in NaN/inf/denormal handling.
- **Run parity tests on SIMD-capable CI hardware.** A runner without AVX2 silently skips the AVX2 path; ensure at least one job exercises it, or the SIMD code is effectively untested.
- For floats, compare with the **same** reduction order, or allow a tolerance — vectorized reductions reassociate and won't be bit-identical to a naive scalar sum.

## Benchmarking: criterion vs divan

- **`criterion`** — mature, statistical, plots/regression detection. Heavier.
- **`divan`** — lighter, fast to write, good for many small parametric benchmarks (vary length, alignment, feature level).

Always benchmark **scalar vs each SIMD variant on the same machine**, and report the `target-cpu`/feature level — a "3x speedup" is meaningless without the baseline build flags. See [../examples/bench_divan.rs](../examples/bench_divan.rs).

## Avoiding UB with `unsafe` intrinsics

- **Feature gate = execution contract.** A `#[target_feature(enable="avx2")]` fn may be *compiled* anywhere but must only be *executed* after a runtime check (`is_x86_feature_detected!`). Executing it on a non-AVX2 CPU is UB (SIGILL). This is why `core::arch` intrinsics are all `unsafe`.
- **Alignment (`std::simd`):** `Simd<T,N>` has the **same shape** as `[T;N]` but **greater alignment** (derived from T *and* N). So:
  - `transmute::<Simd<T,N>, [T;N]>` is zero-cost (lowering alignment is fine).
  - `[T;N] → Simd<T,N>` may need a real copy (raising alignment).
  - Reading/writing `Simd` through a raw pointer obtained from a `T`-aligned slice: use **`read_unaligned`/`write_unaligned`**. The aligned `read`/`write` require full `Simd` alignment; using them on `T`-aligned (under-aligned) data is **UB**.
- **`loadu`/`storeu` (`core::arch`):** prefer the unaligned load/store intrinsics (`_mm256_loadu_ps`) unless you've guaranteed alignment — an aligned `_mm256_load_ps` on misaligned data faults.
- Keep `unsafe` blocks **minimal and annotated** with a `// SAFETY:` note naming the runtime check that justifies them.

## Memory layout & gather/scatter

- **Struct-of-arrays (SoA)** is the SIMD-friendly layout: it both enables autovectorization and avoids gather/scatter. Columnar/Arrow data is already SoA.
- **Alignment matters but modestly** — unaligned arrays can cost ~10–20% in some cases (illustrative, not guaranteed). Use `loadu`/`write_unaligned` for correctness; align hot buffers when profiling shows it pays.
- **Prefer contiguous LOAD over gather/scatter.** Despite one paper claiming gather can match load, the verified guidance is to keep avoiding gather/scatter on the hot path; reserve gather for genuinely random access (e.g. `take` by arbitrary indices, as Vortex does — and even there it's a deliberate, measured choice).
- **SIMD-friendly encodings:** FastLanes bit-packing (Vortex) interleaves values so decode throughput depends only on bit-width, not data content, and auto-vectorizes even from scalar code. When designing a columnar encoding, choose a layout that decodes data-parallel rather than one that forces branch-per-value.

## Feature-flag hygiene

- Gate optional SIMD backends behind Cargo features (`simd-avx512`) so the default build stays portable, but **always keep the scalar path compiled in** — never make correctness depend on a feature being enabled.
- Document the minimum microarch level your release binaries assume (e.g. "requires x86-64-v3"), or ship runtime dispatch so a single binary runs everywhere.
