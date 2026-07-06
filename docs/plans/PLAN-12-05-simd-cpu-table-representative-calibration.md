---
status: executed
date: 2026-07-01
scope: "`horndb-simd` known-CPU ISA table + representative calibration"
---

# 2026-07-01 — `horndb-simd` known-CPU ISA table + representative calibration

## Why

Same-session SPB-256 A/B on **hornbench (AMD Zen4, Ryzen 7 7700)** and **hel01
(Intel Sapphire Rapids, Xeon Gold 5412U)** showed the runtime-calibrated SIMD
kernels are **net-harmful for the real workload** — scalar wins on both hosts:

| config (aggregation-qps) | Zen4 (600 s) | Intel SPR (240 s) |
|---|---|---|
| scalar | **36.0** | **34.4** |
| default (calibrated) | 28.6 | 34.4 |
| all-AVX2 | 28.6 | **17.3** |
| AVX-512 | (double-pumped, slow) | 17.4 |

Findings:
- The microbench "AVX-512 wins 2.5× on Intel" was **fiction for the real
  workload** — AVX-512/AVX2 run at *half* scalar throughput on Intel SPB.
- The dominant culprit is **`lower_bound → AVX2`** (gallop + linear window scan
  vs scalar `partition_point` binary search), called on every leapfrog seek:
  calibration wrongly adopted it on Zen4 (−21 %) but correctly rejected it on
  Intel (Intel default ties scalar). Forcing it on Intel is a 2× cliff.
- Root disease: the balanced, L2-resident micro-calibration inputs are
  unrepresentative of production access patterns, so calibration mis-picks.

**Decision (confirmed with the user):** two-layer kernel selection —
1. a **known-CPU table** keyed by CPUID, populated from real SPB-256
   measurements (authoritative for hosts we've measured), and
2. **representative-input calibration** as the fallback for unlisted CPUs,
   so unknown hosts also pick the workload-correct kernel.

The table is SPB-derived today (both rows scalar-dominated); it has room to grow
per-kernel SIMD entries for other workloads (WCOJ-heavy joins) with their own
measurements. Env overrides (`HORNDB_SIMD_MAX_ISA` / `AUTOTUNE`) and the
intersect skew-gate remain.

## Selection priority (per primitive)

1. `forced_isa()` — test/bench override (unchanged; bypasses everything).
2. `HORNDB_SIMD_MAX_ISA` cap — bounds the candidate set for **all** lower tiers.
3. **Known-CPU table** — if the detected CPU has an entry for this kernel and
   that ISA is within the cap, use it (no timing).
4. **Representative calibration** — `HORNDB_SIMD_AUTOTUNE` on (default): time
   representative inputs, adopt SIMD only if it beats scalar by `MARGIN`.
5. Static widest (autotune off) / scalar baseline.

## Tasks

### Task 1 — CPUID detection + `Kernel` enum + known-CPU table + wiring
- New `cpu.rs`: x86_64 CPUID via `std::arch::x86_64::__cpuid` — vendor (leaf 0),
  family+model with extended-family/extended-model arithmetic (leaf 1). Return
  `Option<CpuKey { vendor: Vendor, family: u32, model: u32 }>`, memoised in a
  `OnceLock`. aarch64 → `None` (no accessible CPUID; falls to calibration).
- Public `Kernel` enum (`Intersect, LowerBound, Merge, Dedup, FilterRange,
  FilterIndicesEq, Gather`) — also lets `serve.rs` drop its string-matching
  (the earlier "public Kernel enum" follow-up).
- `fn table_pick(cpu: CpuKey, k: Kernel) -> Option<Isa>` with rows:
  - `AuthenticAMD` family 25 model 97 (Zen4 Raphael) → `Scalar` for all kernels.
    *(SPB-256: scalar 36.0 vs calibrated 28.6.)*
  - `GenuineIntel` family 6 model 143 (Sapphire Rapids) → `Scalar` for all
    kernels. *(SPB-256: scalar 34.4 vs all-AVX2 17.3.)*
  - Each row cites its SPB-256 basis in a comment.
- Refactor the 7 primitives' `choose()` to the shared priority above: capped
  candidates → table pick (if within cap) → calibrate (autotune) → static widest.
  Keep `resolve()` for the forced path and the `OnceLock<(Isa, Fn_)>` cache.
- Tests (deterministic — no timing asserts): family/model decode from synthetic
  CPUID words; `table_pick` returns the expected ISA for both known keys and
  `None` for an unknown key; selection honours the cap (table pick above cap is
  skipped); forced still bypasses. Cross-arch `cargo check --target
  x86_64-apple-darwin`.

### Task 2 — Representative calibration inputs (fallback correctness)
Fix the mis-picking primitives' `choose()` calibration workloads so an *unlisted*
CPU also rejects the losing kernels:
- `lower_bound`: a **sweep** of many needles spanning a **larger** haystack
  (e.g. N = 65 536, ≥256 needles across the range) — models repeated advancing
  seeks, where scalar binary search wins over the AVX2 linear window scan.
- `gather`: base **larger than L2** (e.g. N = 262 144 u64 = 2 MB) with scattered
  indices — models production column scatter, where the scalar loop beats
  microcoded/slow hardware gather.
- `filter_indices_eq`: representative (moderate) selectivity at larger N, **not**
  the cherry-picked sparse-win shape the current comment admits to.
- `merge`/`dedup`/`filter_range` already pick scalar; leave unless a size bump is
  trivial. Update the benches to mirror the representative shapes so they can't
  false-green again. Keep the intersect skew-gate note accurate.

### Task 3 — `source` label on `horndb_simd_kernel_isa`
- Add a `SimdSource` label enum (`table`, `calibrated`, `static`, `forced`) so
  the metric shows **why** an ISA was chosen. Expose the source from
  `calibration_report()` (return `(name, Isa, Source)`), map it in `serve.rs`
  using the new `Kernel` enum, update `docs/metrics.md` **in the same commit**.

### Task 4 — Docs + benchmarks sync
- `lib.rs` module doc: the selection priority + table + representative calibration.
- `crates/simd/AGENTS.md`: the SPB-256 cross-host finding, the table, and the
  representative-calibration rule (supersede the balanced-only-calibration note
  where needed — it was for intersect's gate; calibration inputs elsewhere must
  be representative).
- `docs/benchmarks.md`: record the cross-host SPB-256 ISA sweep (Zen4 + Intel).
- `TASKS.md` + `docs/architecture.md`: sync the SPEC-12 (#132) state; mirror to
  the GH issue.

## Verification (both hosts)
- Rebuild `serve`; confirm `calibration_report()` shows the table's scalar picks
  on both hosts (source = `table`), and SPB-256 recovers: Zen4 ~36, Intel ~34.
- Confirm `HORNDB_SIMD_MAX_ISA=scalar` still forces scalar; confirm an unlisted
  CPU (simulate via a test hook, or reason) uses representative calibration.
- `cargo nextest run -p horndb-simd`; `cargo test -p horndb-wcoj -p
  horndb-storage`; `cargo check --target x86_64-apple-darwin -p horndb-simd
  --all-targets`; `cargo clippy -p horndb-simd --all-targets -- -D warnings`.
- Never `cargo clippy --workspace` (oxrocksdb).
