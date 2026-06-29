# `horndb-simd` (SPEC-12) — agent notes

Dependency-free leaf crate of runtime-dispatched SIMD primitives (`intersect`,
`merge`, `dedup`, `lower_bound`, `gather`, `filter*`) over primitive slices. Each
primitive resolves the widest kernel the host supports (scalar / AVX2 / AVX-512 /
NEON) once, caching a function pointer, and is proven bit-identical to the scalar
oracle by the differential proptest in `tests/differential.rs`. This is the **only**
crate allowed to carry hand-written SIMD intrinsics. See the `src/lib.rs` module doc
for the dispatch story and the `HORNDB_SIMD_MAX_ISA` operational cap.

## ⚠️ Cross-arch false-green — read before trusting a local green run

**CI runs on x86_64 (`ubuntu-latest`); the dev laptop is aarch64.** The AVX2 and
AVX-512 kernels therefore **never execute on the laptop** — `tests/differential.rs`
builds its ISA list with `is_x86_feature_detected!`/`is_aarch64_feature_detected!`,
so on aarch64 it only forces the **Scalar + NEON** paths. A bug in an x86 kernel
(e.g. the shipped `dedup` AVX2 `u64::MAX` wrapping-overflow) passes locally and
fails **only on Intel CI**. The scalar kernels in `src/scalar.rs` are the oracle.

Before claiming any x86 SIMD kernel correct:

- Don't treat a local green as proof — the x86 differential arms didn't run.
- Force every ISA explicitly via `with_forced_isa(Isa::Avx2 | Isa::Avx512, …)` and,
  for x86 coverage, run on an x86_64 host (CI, or `HORNDB_SIMD_MAX_ISA` on a real
  Intel box). A CI x86 failure here is **signal, not flakiness**.
- Boundary values (`0`, `u64::MAX`, empty/one-element slices) are the usual
  killers — add them as explicit differential cases, not just proptest-random.

## Build & test

```bash
cargo nextest run -p horndb-simd                         # scalar + host-native ISA only
cargo bench -p horndb-simd --bench intersect             # record on hornbench, not the laptop
```
