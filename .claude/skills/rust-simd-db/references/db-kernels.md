# Database kernels — which technique per operation

Mapping common columnar/DB kernels to the right SIMD layer. Items marked **(verified)** rest on primary sources; items marked **(design guidance)** are reasoned from the architecture but were not benchmarked in this skill's research — treat them as starting points, measure before trusting.

## Arithmetic on columns (add/sub/mul) — **autovec (verified)**

Simple elementwise. **Use autovectorization**, not hand-written SIMD: arrow-rs deleted its manual SIMD kernels because autovec was ~23–46% faster. Division/modulo were the exception arrow kept manual — autovec handles those less well, so benchmark before assuming. SoA layout + iterator (not indexed) loops. See examples/autovec_add.rs.

## Filter / selection (predicate → mask/bitmap) — **mixed**

- **Comparison producing a boolean/mask column** is elementwise → often autovec-friendly. `a[i] < threshold` over a sliced loop tends to vectorize.
- **Packing the mask into a bitmap** (one bit per row) and **selection-vector materialization** are irregular — the bit-packing step and the compaction (keep only passing rows) generally need explicit SIMD (e.g. `movemask`-style extraction on x86) to go fast. **(design guidance)** — research evidence here was thin; prototype against the scalar path and measure.

## `take` / gather by arbitrary indices — **intrinsics + dispatch (verified)**

Random-index access. This is the textbook case where intrinsics beat autovec. Vortex dispatches `is_x86_feature_detected!("avx2")` to an AVX2 gather kernel (`_mm256_mask_i32gather_epi32`) with `take_primitive_scalar` as fallback (also used for sub-32-bit and >8-byte values). Gather is the *deliberate* exception to "avoid gather" — justified because the access is genuinely random. See examples/dispatch_take.rs.

## Bitpacking / integer decode — **SIMD-friendly layout (verified)**

Don't hand-vectorize a naive bit-unpacker; choose a **layout designed to decode data-parallel**. FastLanes bit-packing (used by Vortex) interleaves values so decode speed depends only on bit-width and auto-vectorizes even from scalar code — the FastLanes paper reports >100 billion integers/sec with scalar code on that layout. Lesson: **encoding choice dominates kernel cleverness** for decode throughput.

## Dictionary / RLE decode — **intrinsics or SIMD-friendly layout (design guidance)**

Dictionary decode is effectively a `take` from the dictionary by code → same gather considerations as `take`. RLE expansion is irregular. Prefer layouts/encodings that decode data-parallel; fall back to intrinsics + scalar fallback. Not separately benchmarked here.

## Varint / LEB128 decode — **intrinsics (design guidance)**

Branchy, variable-length, byte-serial — hostile to autovec. Fast implementations use SIMD (e.g. `movemask` over continuation bits) but are intricate. Consider whether a fixed-width or FastLanes-style encoding avoids the problem entirely before investing in a SIMD varint decoder.

## String / byte-slice comparison — **intrinsics (design guidance)**

- Equality/prefix scans can use 16/32-byte vector compares (`_mm256_cmpeq_epi8` + `movemask`). Note autovectorized string compare **cannot early-exit on first mismatch** the way scalar does, so for short or early-differing strings scalar may win — measure on representative data.
- AVX2 matters here because it extended 256-bit ops to integers (byte-level work).

## Hash / aggregation — **mixed (design guidance)**

- **Aggregations** (sum/min/max/count over a column) are reductions — autovec handles simple ones; watch float reassociation (vectorized sums differ from scalar order).
- **Hashing for group-by** is largely data-dependent/irregular; SIMD hashing exists but is specialized. Start scalar, profile, then consider SIMD only for proven hot paths.

---

## Cross-cutting takeaways

1. **Layout beats kernel cleverness.** SoA + a decode-friendly encoding (FastLanes) gets you most of the way, often via plain autovec.
2. **Gather is the deliberate exception**, not a default — use it only for genuinely random access (`take`, dict decode), behind dispatch + fallback.
3. **The irregular kernels (take, bitpack, varint, string) are where intrinsics earn their keep** — and where the scalar-parity test matters most.
