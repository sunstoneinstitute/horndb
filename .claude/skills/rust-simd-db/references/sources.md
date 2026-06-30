# Sources & confidence

Every recommendation in this skill traces to one of these. Confidence reflects adversarial verification (3-vote): "high" = confirmed against primary sources.

## Primary (authoritative)

- **arrow-rs PR #1221** — removed hand-written SIMD arithmetic kernels because autovectorization was ~46% faster (add), ~23% (sub), ~25% (mul) on Cascade Lake with `target-cpu=native`; division/modulo kept manual. The `simd` feature was later removed entirely. https://github.com/apache/arrow-rs/pull/1221 — *high*
- **std::simd docs** — `Simd<T,N>` is a nightly-only experimental API (`portable_simd`, #86656); "compiles for every target, unlike std::arch"; layout/alignment + `read_unaligned`/`write_unaligned` UB rules. https://doc.rust-lang.org/std/simd/struct.Simd.html — *high*
- **core::arch docs** — all intrinsics are `unsafe`; calling an AVX2 fn on a non-AVX2 CPU is UB. https://doc.rust-lang.org/core/arch — *high*
- **is_x86_feature_detected! docs** — runtime CPU feature detection, stable since 1.27. https://doc.rust-lang.org/std/macro.is_x86_feature_detected.html — *high*
- **Rust 1.86.0 release blog** — `target_feature 1.1` stabilized (RFC 2396, PR #116114): gated fns can call same/superset-gated fns safely. https://blog.rust-lang.org/2025/04/03/Rust-1.86.0/ — *high*
- **multiversion crate docs** — proc-macro multiversioning + runtime dispatch; "executing an unsupported instruction will result in a crash." https://docs.rs/multiversion — *high*
- **target-feature-dispatch crate docs** — declarative, expression-position, no CPU-model DB, caches resolved fn pointer via `OnceLock`. https://docs.rs/target-feature-dispatch — *high*
- **RFC 2045 (target-feature)** — compiler won't emit vector instructions beyond baseline unless statically known supported. https://rust-lang.github.io/rfcs/2045-target-feature.html — *high*
- **Vortex repo** — `vortex-array/src/arrays/primitive/compute/take/{mod.rs,avx2.rs}`: `is_x86_feature_detected!("avx2")` → `_mm256_mask_i32gather_epi32` gather kernel under `#[target_feature(enable="avx2")]`, scalar fallback otherwise. https://github.com/vortex-data/vortex — *high*
- **FastLanes paper** (Afroozeh & Boncz, VLDB 16:2132) — bit-packing layout where decode speed depends only on bit-width, auto-vectorizes even with scalar code. — *high*

## Blog / secondary (illustrative)

- **Shnatsel, "State of SIMD in Rust 2025"** — when to pick autovec vs intrinsics vs std::simd; x86 baseline SSE2; recommends `wide`/`pulp`/`macerator` on stable. https://shnatsel.medium.com/the-state-of-simd-in-rust-in-2025-32c263e5f53d — *high*
- **nickwilcox, "autovec"** — audio-mixing benchmark: autovec 25.535 µs ≈ intrinsics 25.781 µs vs scalar 77.67 µs (~3x), no `unsafe`; bounds-check blocking. https://www.nickwilcox.com/blog/autovec/ — *high (single self-reported non-DB kernel — illustrative)*
- **Linebender, "Towards Fearless SIMD"** — detect once at startup + pass token; target_feature 1.1. https://linebender.org/blog/towards-fearless-simd/ — *high*
- **curiouscoding, "Distributing Rust SIMD binaries"** — don't ship `target-cpu=native`; target a microarch level (x86-64-v3). https://curiouscoding.nl/posts/distributing-rust-simd-binaries/ — *high*
- **simd_aligned crate** — unaligned arrays cost ~10–20% (single self-serving anecdote). https://lib.rs/crates/simd_aligned — *low (illustrative only)*

## Refuted — do NOT repeat these

- ❌ "GATHER can match LOAD performance when applied properly" — vote 1-2. Keep the conventional avoid-gather/scatter guidance. (Springer s13222-022-00431-0)
- ❌ "Stable rustc won't autovectorize float operations" — vote 0-3. **False** — stable does autovectorize floats, subject to reassociation/fast-math constraints. (misread of Shnatsel 2025)

## Open questions (not yet verified)

- Best stable-Rust portable SIMD crate for DB kernels (`wide` vs `pulp` vs `macerator` vs `simdeez`) — relative perf/ergonomics unverified here.
- Concrete SIMD techniques for predicate→bitmap, vectorized hashing, SIMD string compare — evidence thin; treat `db-kernels.md` sections on these as design guidance, not benchmarked fact.
- Whether `std::simd` stabilized after the Jan 2026 knowledge cutoff — **re-check before asserting nightly-only**.
