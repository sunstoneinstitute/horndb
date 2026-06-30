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

## `intersect`: skew gate — benches MUST include skew; calibration stays balanced

`intersect` selects **galloping** (scalar, `O(|small|·log|large|)`) for skewed
size ratios and the **block-SIMD** kernels (`O(|large|)`) for balanced inputs,
gated by `GALLOP_RATIO` (= 16) on `max(|a|,|b|) / min(|a|,|b|)`. Leapfrog feeds
`intersect` skewed `active_run`s (e.g. 3 keys vs 50 000), so the skewed regime is
the common production case.

**Benches MUST include skewed shapes, not just balanced ones.** A balanced-only
bench (`make_runs(4096)`) false-greened a −7% SPB-256 aggregation-qps regression
(bisected to `ccecd5f`, which replaced galloping with block-only on every arm);
block was 50–2000× slower on skew. Benches feed the *unforced* `auto` path, so
they must carry skew to keep that regression visible. Skew bench coverage lives
in `benches/intersect.rs`; correctness coverage in `tests/skew_intersect.rs`.

**Calibration (`calib_input`, intersect.rs, N=4096) MUST stay balanced-only —
this is intentional, do NOT "fix" it to add a skewed shape.** Calibration picks
the fastest *block kernel* and bypasses the gate (it invokes the block kernels
directly). Post-gate, the calibrated block kernel only ever runs on **balanced**
inputs in production — the skew path is the uncalibrated scalar gallop, which has
no kernel to choose. Calibrating on a skewed shape would benchmark the block
kernels on data they never see in production and pick a kernel that never runs on
skew: a net regression. Balanced-only calibration matches what production
actually dispatches.

The `forced_isa` bypass routes straight to the forced block kernel and skips the
gate on purpose — the force is a test/bench affordance to exercise one specific
kernel; the gate is production-only. So `tests/differential.rs` (which forces
each ISA) still covers the block kernels, and `tests/skew_intersect.rs` (which
runs *unforced*) covers the galloping path.

## Build & test

```bash
cargo nextest run -p horndb-simd                         # scalar + host-native ISA only
cargo bench -p horndb-simd --bench intersect             # record on hornbench, not the laptop
```
