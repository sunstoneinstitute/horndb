# SIMD ISA support research — AMD/Intel server CPUs (last ~6 years)

Deep-research pass (2026-07-01) to support `cpu.rs::table_pick` (the known-CPU
table described in `AGENTS.md`). 103 agents, 21 sources, 92 claims extracted,
25 adversarially voted (3 votes/claim, ≥2 refutes kills). Full raw output
(confirmed + refuted claims, sources, vote tallies) is not checked in here —
ask the session that ran it if you need the raw JSON.

**Headline: `avx512f`-family flags in `/proc/cpuinfo` are not a reliable proxy
for real throughput.** Whether AVX-512 helps, hurts, or is a wash vs AVX2 is
workload/kernel-shape dependent even *within* a single CPU generation — this
is consistent with what `AGENTS.md` already found on hornbench (SIMD net-harmful
on the real SPB-256 workload despite winning microbenches). A static
CPU-family→ISA table is necessary but not sufficient; per-kernel calibration
(already implemented) is what actually catches the variance.

## ISA support by generation

| Vendor | Microarch (example parts) | AVX-512 datapath | Reported `/proc/cpuinfo` flags (subset) | Real-world behavior | Confidence |
|---|---|---|---|---|---|
| AMD | Zen 1–3 (Naples/Rome/Milan) | none | no `avx512*` flags at all | n/a — AVX2 is the ceiling | high (undisputed) |
| AMD | **Zen 4** (Genoa EPYC 9004, Bergamo, Raphael/Ryzen 7000 — incl. hornbench's Ryzen 7 7700, `family 25 / model 97` per `cpu.rs`) | **double-pumped**: 512-bit ops split into 2× sequential 256-bit micro-ops on Zen 3's 256-bit units, *except* a genuinely native 512-bit shuffle/permute unit | `avx512f avx512vl avx512dq avx512bw avx512cd avx512ifma avx512vbmi avx512vbmi2 avx512vpopcntdq avx512bitalg avx512vnni avx512bf16 vpclmulqdq gfni vaes` — **no `avx512_vp2intersect`, no `avx512fp16`** | No Intel-style throttling. Speedup vs AVX2 is **workload-dependent**: ~2.3% on x265 (arithmetic-heavy) up to +14–33% on simdjson / +15.6% on Embree (shuffle/permute-heavy). Matches hornbench SPB finding that `intersect`/`gather` stay net-neutral-to-AVX2 but `lower_bound` regresses badly under AVX2/NEON. | high |
| AMD | **Zen 5** (Turin EPYC 9005, Granite Ridge) | **native** full 512-bit execution — AMD's first true-width implementation | adds `avx512fp16`? and `avx512vp2intersect` relative to Zen4 (not independently confirmed this pass — verify against real hardware) | Still has a real, measurable penalty: 512-bit **loads** (memory-touching AVX-512 ops) dip clocks to ~4.7 GHz for roughly a dozen ms before recovering — milder than Intel Skylake-SP's historical drop but nonzero. AMD reportedly exposes a BIOS option to force double-pumped 256-bit mode for power efficiency, so CPUID-native-512 ≠ guaranteed native-width execution at runtime. | medium (2-1 votes) |
| Intel | Skylake-SP / Cascade Lake (Xeon Scalable 1st/2nd gen, e.g. Xeon Silver 4116) — **2017, at/past the edge of the 6-yr window, historical context only** | native, but license-gated | `avx512f avx512cd avx512dq avx512bw avx512vl` (+ VNNI on Cascade Lake) | "Light" vs "heavy" 512-bit instruction classes each carry a separate max-frequency license; heavy AVX-512 measured cutting a Xeon Silver 4116 from 2.1 GHz base to ~1.4 GHz all-core (~33%). This is the historical origin of "AVX-512 throttles" folklore — do not extrapolate it to current Intel parts. | medium (2-1 on the specific numbers; licensing mechanism itself 3-0) |
| Intel | Ice Lake-SP onward | native | broader `avx512*` subset (adds IFMA/VBMI/VBMI2/VPOPCNTDQ/BITALG/VNNI/GFNI/VAES vs Skylake-SP) | Intel progressively relaxed the light/heavy licensing distinction starting here | **not independently confirmed this pass** — claims attempted were refuted on verification (weak sourcing), needs fresh research |
| Intel | **Sapphire Rapids** (Xeon Gold 5412U on hel01, `family 6 / model 143` per `cpu.rs`) | native | — | — | **not confirmed this pass** — Wikipedia-sourced claim about full AVX-512+AMX+AVX-VNNI feature set was refuted (1-2 votes); don't cite until re-verified against a real cpuinfo dump or primary Intel doc |
| Intel | Emerald Rapids, Granite Rapids | native | — | — | **not confirmed this pass** — Granite Rapids AVX-512-FP16 claim refuted (0-3); needs separate research |

## What's solid enough to encode now

1. **Zen 4 has no reliable single-flag AVX-512-good/bad signal** — the existing
   `cpu.rs` row (Zen4 → scalar for all kernels, from real SPB-256 measurement)
   is the right approach: measured-per-kernel beats ISA-flag-per-CPU. The
   research reinforces that this must stay a *per-kernel* table entry, not a
   blanket "Zen4 = no AVX-512" rule, since shuffle/permute-shaped kernels could
   plausibly benefit from Zen4's native permute unit even though `lower_bound`/
   `intersect` don't.
2. **Zen 5 is not simply "Zen4 but faster AVX-512."** It's genuinely
   native-width, but still pays a load-triggered frequency transient, and can
   apparently be BIOS-configured back into double-pumped mode. If/when a Zen5
   table row is added, it needs its own SPB-style measurement — don't assume
   Zen4's scalar-wins verdict transfers, and don't assume "native 512-bit"
   means AVX-512 is now free either.
3. **Skylake-SP-style light/heavy licensing throttling is legacy** and
   shouldn't be assumed present on current Intel Xeon Scalable generations
   (Ice Lake-SP and later) without separate confirmation.

## Gaps — needs follow-up before trusting for table rows

- No source in this pass captured an actual `/proc/cpuinfo` dump from real
  hardware; the flag lists above are vendor-doc/Wikipedia-sourced. Spot-check
  against hornbench (Zen4) and hel01 (Sapphire Rapids) directly, e.g.
  `grep -o 'avx512[a-z0-9_]*' /proc/cpuinfo | sort -u`.
- Sapphire Rapids / Emerald Rapids / Granite Rapids AVX-512 characteristics
  (native-vs-throttled, exact subset) are **unconfirmed** — every claim
  attempted on post-Skylake-SP Intel generations failed adversarial
  verification in this pass (weak/contradicted sourcing), not because the
  underlying facts are false, just under-sourced. Re-research with better
  primary sources (Intel ISA extension manuals, `lscpu`/cpuid dumps) before
  adding table rows for these.
- No confirmed data on Zen4c ("Bergamo," dense cores) or Zen5c specifically.
- Open question worth testing empirically on hornbench: does the Zen4
  workload split (modest win for arithmetic-heavy, large win for
  shuffle/permute-heavy) correlate with which primitives use permute/shuffle
  internally? If so, that's a principled reason to special-case Zen4's
  `intersect`/`gather` differently from `lower_bound` in `cpu.rs`, rather than
  uniformly scalar.
