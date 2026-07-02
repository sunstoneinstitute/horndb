# `horndb-simd` (SPEC-12) — agent notes

Dependency-free leaf crate of runtime-dispatched SIMD primitives (`intersect`,
`merge`, `dedup`, `lower_bound`, `gather`, `filter*`) over primitive slices. Each
primitive resolves the widest kernel the host supports (scalar / AVX2 / AVX-512 /
NEON) once, caching a function pointer, and is proven bit-identical to the scalar
oracle by the differential proptest in `tests/differential.rs`. This is the **only**
crate allowed to carry hand-written SIMD intrinsics. See the `src/lib.rs` module doc
for the dispatch story and the `HORNDB_SIMD_MAX_ISA` operational cap, and
`docs/architecture/simd.md` for the subsystem-level guide (selection ladder,
consumers, env-var knobs).

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

## Kernel selection: known-CPU table first, representative calibration second

**SIMD is net-harmful for the real workload on every host we've measured.** A
same-session LDBC SPB-256 `aggregation-qps` A/B on **hornbench (AMD Zen4, Ryzen 7
7700)** and **hel01 (Intel Sapphire Rapids, Xeon Gold 5412U)** showed scalar wins
on both. Do NOT compare qps *across* the two columns — the measurement windows
differ (Zen4 600 s, Intel 240 s); compare only *within* a column:

| config (aggregation-qps) | Zen4 hornbench (600 s) | Intel SPR hel01 (240 s) |
|---|---|---|
| pre-SIMD baseline | 30.6 | — |
| SIMD tip, calibrated (pre-fix) | 28.6 | 34.4 |
| all-AVX2 forced | 28.6 | 17.3 |
| AVX-512 forced | — (Zen4 double-pumped, slow) | 17.4 |
| **fix: known-CPU table → all scalar** | **36.16** | 34.4 |

- **Culprit: `lower_bound → AVX2/NEON`** (gallop + linear window scan) losing to
  scalar `partition_point` binary search on the seek-heavy leapfrog path.
  Calibration wrongly adopted it on Zen4 (−21%) while Intel's calibration already
  tied scalar.
- Zen4: the table fix recovers to **36.16** (+26% over the 28.6 regression, +18%
  over the 30.6 pre-SIMD baseline).
- Intel: ~unchanged (34.4) — its calibration had already picked scalar
  `lower_bound`; the table now makes that deterministic/table-sourced and avoids
  the all-AVX2 2× cliff (17.3).
- The earlier microbench claim *"AVX-512 `intersect` wins ~2.5× on Intel"* was
  **microbench-only and is contradicted by the real workload** — AVX-512 runs at
  roughly half scalar throughput on SPB. Don't cite it as a shipping win.

**Selection is two-layer** (full priority in the `src/lib.rs` module doc):

1. **Known-CPU table** (`cpu.rs`, `table_pick`) — CPUID vendor/family/model →
   per-kernel `Isa`, populated from real SPB-256 measurements. A hit selects with
   **no timing**. Rows today: `AuthenticAMD` family 25 model 97 (Zen4) and
   `GenuineIntel` family 6 model 143 (Sapphire Rapids), both → scalar for all
   kernels.
2. **Representative-input calibration** — fallback for an unlisted CPU. Verified
   on Zen4 with the table bypassed, it picks `lower_bound → Scalar` and
   `filter_indices_eq → Scalar` (both were AVX2 before) while `intersect`/`gather
   → Avx2` remain (SPB-neutral), so an unknown host also rejects the killer
   kernel.

The chosen tier is recorded as a [`Source`] (table/calibrated/static) on
`calibration_report()`, the `horndb_simd_kernel_isa{source=…}` metric label, and
the `serve` startup log.

### Adding a CPU or kernel table row
1. Measure SPB-256 `aggregation-qps` on the host (see the repo's "run benchmarks
   on hornbench" rule), A/B the candidate ISA against scalar.
2. Add a match arm in `cpu.rs::table_pick` keyed on `(Vendor, family, model)`,
   returning the winning `Isa` per `Kernel`. Cite the SPB-256 basis in a comment,
   as the existing rows do.
3. Sync `BENCHMARKS.md`, `docs/architecture.md`, and `TASKS.md`.

### Representative-calibration rule (applies to every primitive EXCEPT intersect)
Calibration inputs MUST reflect production access patterns, or calibration
mis-picks (this is the root cause above): **seek-sweep** for `lower_bound` (many
needles over a haystack larger than L2), **>L2 base** for `gather` (scattered
indices over a base bigger than L2), **moderate selectivity** for
`filter_indices_eq`. Mirror these shapes in the benches so they can't false-green.
This does **not** override the intersect skew-gate note below — `intersect`'s
*balanced-only* calibration is correct precisely because the skew-gate handles the
skewed regime with an uncalibrated scalar gallop.

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
