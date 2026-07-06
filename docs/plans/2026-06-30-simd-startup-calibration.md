# 2026-06-30 — `horndb-simd` startup micro-calibration

## Why

Hornbench (Zen4 EPYC) vs hel01 (Xeon Gold 5412U, Sapphire Rapids) benches showed
the per-ISA SIMD kernels win or lose **depending on the host**, with no cheap
runtime bit to tell the cases apart:

| kernel | scalar | AVX2 | AVX-512 | note |
|---|---|---|---|---|
| intersect (Intel, n=4096) | 1.02 G | 1.77 G | **2.57 G** | AVX-512 2.5× win |
| intersect (Zen4, n=4096) | 2.16 G | 2.31 G | **0.87 G** | AVX-512 2.5× *loss* (double-pumped) |
| lower_bound (both archs) | best | 2–11× slower | 2–11× slower | linear-scan loses to binary search everywhere |
| gather (both) | 1× | 1.5–2.2× | 1.5–2.2× | win |
| filter_indices_eq sparse | 1× | ~1.9× | ~1.9× | win; dense ≈ parity |

A static ISA preference is wrong on at least one host. A CPU-model blocklist
(detect Zen4) is a maintenance treadmill that misses "similar cases"
(downclocking, future double-pumped parts). **Decision: pick the fastest kernel
per primitive by timing them at startup**, default on, overridable via
`HORNDB_SIMD_*`. This auto-selects AVX-512 intersect on Intel, AVX2/scalar on
Zen4, scalar for `lower_bound` everywhere — with zero hardcoded verdicts.

## Design

### New module `crates/simd/src/calibrate.rs`
- `const MARGIN: f64 = 0.05` — only switch off scalar if a SIMD kernel beats it
  by ≥5% (encodes "earn your intrinsics"; damps noise). `const WARMUP: u32 = 3`,
  `const ITERS: u32 = 7` (min-of-7).
- `fn time_kernel<P: Copy>(k: P, run: &impl Fn(P)) -> Duration` — WARMUP runs,
  then min elapsed over ITERS. Uses `std::time::Instant` and
  `core::hint::black_box` on the workload output.
- `pub(crate) fn pick<P: Copy>(candidates: &[(Isa, P)], run: impl Fn(P)) -> (Isa, P)`
  — `candidates[0]` is the scalar baseline by contract. Time scalar, time each
  other candidate; return the fastest candidate that beats scalar by ≥ MARGIN,
  else scalar. Never returns a candidate outside the slice.

### Env knobs (in `dispatch.rs`, same memoised-OnceLock style as `isa_cap`)
- **`HORNDB_SIMD_MAX_ISA`** (existing): caps the candidate *set* (calibration
  only considers ISAs ≤ cap). Unchanged.
- **`HORNDB_SIMD_AUTOTUNE`** (new): `off`/`0`/`false`/`no` → calibration
  disabled, fall back to the current static-preference `resolve()`; anything
  else / unset → enabled (default). `pub(crate) fn autotune_enabled() -> bool`,
  memoised. Expose `pub fn configured_autotune() -> bool` for startup logging
  (mirrors `configured_max_isa`).

### Per-primitive wiring (intersect, lower_bound, merge, dedup, filter_range,
### filter_indices_eq, gather)
Keep `resolve()` exactly as today (it serves the `forced_isa` arms AND the
autotune-off fallback). Change only the production cache:
- Cache type becomes `OnceLock<(Isa, Fn_)>` (store the chosen ISA for reporting).
- `dispatch()`: if `forced_isa().is_some()` → `resolve()` (unchanged, bypasses
  calibration so tests stay deterministic); else return `CACHE.get_or_init(choose).1`.
- New `fn choose() -> (Isa, Fn_)`:
  - if `!autotune_enabled()` → derive `(isa, fn)` from `resolve()`'s static
    preference (return `(Isa::Scalar, scalar::…)` when resolve picks scalar, etc.
    — simplest: have `choose` build the same candidate list and, when autotune
    off, return the *last* (widest) candidate, which equals today's preference).
  - else build `candidates` = scalar first, then each host-supported ISA that
    `crate::dispatch::allows(..)` permits and is feature-detected, then
    `calibrate::pick(&candidates, |f| run_workload(f))`.
  - `run_workload` builds a fixed deterministic L2-resident synthetic input
    (mirror the bench shapes; n≈4096) and runs the kernel into a black-boxed,
    reused output. Inputs built once outside the timing loop.
- `merge`'s only non-scalar kernel currently delegates to scalar, so its
  candidate list is effectively scalar-only on most hosts — calibration will
  just return scalar; that's fine (no special-casing).

### Public API (`lib.rs`)
- `pub fn init()` — run calibration for every primitive now (call each
  primitive's `pub(crate) fn prime()` which does `let _ = dispatch();`). Hosts
  call this at startup after logging policy; lazy first-use still works if they
  don't.
- `pub fn calibration_report() -> Vec<(&'static str, Isa)>` — `(primitive name,
  chosen ISA)`; reads each primitive's cache (triggering calibration if needed).
- Re-export `configured_autotune`.
- Update the module doc to describe calibration + the two env knobs.

## Tasks

1. **Calibration engine + full wiring + tests.** Add `calibrate.rs`; add
   `HORNDB_SIMD_AUTOTUNE` parsing + `autotune_enabled()`/`configured_autotune()`
   in `dispatch.rs`; convert all seven dispatched primitives to the
   `OnceLock<(Isa, Fn_)>` + `choose()` pattern; add `init()`,
   `calibration_report()`, exports, module-doc. Tests: `pick` chooses the faster
   synthetic kernel and honours MARGIN (scalar wins a tie / marginal case);
   `autotune_enabled` parses env; with autotune **off** the static preference is
   used; `init()`+`calibration_report()` return one entry per primitive within
   the cap; a calibrated kernel still matches the scalar oracle (the existing
   `tests/differential.rs` already covers per-ISA correctness via
   `with_forced_isa`, which calibration must not disturb).

2. **Expose selected ISA via metrics.** In `crates/metrics/`: add `SimdMetrics`
   — a `Family<SimdKernelLabel, Gauge>` named `simd_kernel_isa` (scrapes as
   `horndb_simd_kernel_isa{primitive,isa}`), an info-gauge set to `1` for the
   selected `(primitive, isa)` series. Add typed label enums to `labels.rs`:
   `SimdKernel` (intersect/lower_bound/merge/dedup/filter_range/
   filter_indices_eq/gather) and `SimdIsa` (scalar/avx2/avx512/neon), plus the
   `SimdKernelLabel { kernel, isa }` set. Register in `MetricsState`. The metrics
   crate must **not** depend on `horndb-simd`. Add a `record(kernel, isa)` setter.
   Glue: `crates/sparql/src/bin/serve.rs` (add a direct `horndb-simd` dep) calls
   `horndb_simd::init()` at startup, then maps each `calibration_report()` entry
   (`(&str, horndb_simd::Isa)`) to `SimdMetrics::record`. Update `docs/metrics.md`
   with the new series row in the **same commit**.

3. **Docs sync.** `docs/benchmarks.md`: record the hel01 (Intel) column next to the
   Zen4 numbers and replace the hand-tuned RED/GREEN verdicts with the
   calibration outcome (dispatch now auto-selects per host; the floor-miss on
   intersect is noted but no longer drives a manual default). `lib.rs`/`dispatch.rs`
   docs: the two env knobs. `docs/architecture.md` SPEC-12 row + `TASKS.md`
   (#132) flipped to reflect calibration shipped; mirror to the GH issue.

## Verification (dev host is aarch64 — x86 kernels need cross-check)
- `cargo test -p horndb-simd` (or nextest) — scalar + NEON, incl. differential.
- `cargo check --target x86_64-apple-darwin -p horndb-simd --all-targets`.
- `cargo clippy -p horndb-simd --all-targets -- -D warnings` (host + x86 target).
- `cargo test -p horndb-wcoj -p horndb-storage` — consumers (calibration now
  runs in production seek/intersect paths).
- Re-run the four benches on hel01 **and** hornbench afterwards to confirm
  calibration picks AVX-512 on Intel, AVX2/scalar on Zen4, scalar lower_bound.
- Never `cargo clippy --workspace` (pulls oxrocksdb); scope to `-p horndb-simd`.
