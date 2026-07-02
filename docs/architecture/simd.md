# SIMD Dispatch & Kernel Selection — Architecture Guide

How `horndb-simd` (SPEC-12) turns a handful of hot inner loops into
runtime-dispatched SIMD kernels: what the crate offers, where the rest of the
engine plugs it in, how a kernel is *chosen* per host (the part that is far
subtler than the intrinsics themselves), and the two environment variables that
let ops override that choice without a rebuild.

Read this before touching kernel selection, adding a CPU table row, or wiring a
new consumer onto a primitive. For the *why* (the SPEC-03 `≤2.5 ns/tuple` gap
that motivated the layer) and the acceptance criteria, see
`docs/specs/SPEC-12-simd.md`. For the contributor gotchas — the cross-arch
false-green and the skew-gate rules — see the crate's `AGENTS.md` (the
`CLAUDE.md` symlink). For the raw CPU research behind the known-CPU table, see
`crates/simd/simd-research-findings.md`.

## 0. The one thing to internalise

**SIMD is net-*harmful* for HornDB's real workload on every host we have
measured.** A same-session LDBC SPB-256 `aggregation-qps` A/B on both AMD Zen4
(hornbench) and Intel Sapphire Rapids (hel01) showed **scalar wins on both**.
The balanced, L2-resident inputs the kernels look good on in microbenchmarks are
not the skewed, memory-bound shapes production actually dispatches. The dominant
culprit is `lower_bound` (gallop + linear window scan) losing to scalar
`partition_point` binary search on the seek-heavy leapfrog path.

So the interesting engineering here is *not* "we wrote fast AVX-512 kernels." It
is the **selection machinery** that keeps those kernels from being adopted where
they lose — a known-CPU table backed by real measurements, a calibration
fallback with production-representative inputs, a per-`intersect` skew gate, and
two operational override knobs. The rest of this guide is that machinery.

## 1. The crate: a dependency-free leaf of primitives

`horndb-simd` is the **only** crate in the workspace allowed to carry
hand-written SIMD intrinsics. It is a dependency-free leaf: every public function
is a safe wrapper over `&[u64]`/`&[u32]` slices that dispatches once to a
scalar / AVX2 / AVX-512 / NEON kernel and is proven **bit-identical to the
scalar oracle** (`src/scalar.rs`) by the differential proptest in
`tests/differential.rs`.

### 1.1 The seven primitives

Exported from `src/lib.rs` (one module each); the `Kernel` enum (`cpu.rs`) names
them for selection and metrics:

| Primitive | Signature | `Kernel` | Kernels today |
|---|---|---|---|
| `intersect` | `(a: &[u64], b: &[u64], out: &mut Vec<u64>)` | `Intersect` | scalar gallop + block-SIMD (AVX2/AVX-512/NEON), **skew-gated** (§4) |
| `lower_bound` | `(haystack: &[u64], value) -> usize` | `LowerBound` | scalar `partition_point` + gallop/SIMD block compare |
| `merge` | `(a: &[u64], b: &[u64], out: &mut Vec<u64>)` | `Merge` | dispatched; scalar-backed today |
| `dedup` | `(sorted: &[u64], out: &mut Vec<u64>)` | `Dedup` | scalar + SIMD run-skip |
| `filter` | `(values: &[u64], keep: impl Fn(u64)->bool, out)` | `FilterRange`* | **always scalar** — a closure can't cross a `#[target_feature]` boundary |
| `filter_range` | `(values: &[u64], lo, hi, out: &mut Vec<u64>)` | `FilterRange` | scalar + AVX2/NEON range filter |
| `filter_indices_eq` | `(values: &[u64], needle, out: &mut Vec<u32>)` | `FilterIndicesEq` | scan + compact matching indices |
| `gather` | `(base: &[u64], indices: &[u32], out: &mut Vec<u64>)` | `Gather` | indexed load (`vpgatherqq` / scalar) |

`filter` and `filter_range` share the `FilterRange` kernel slot; `filter` itself
never vectorises (the predicate closure blocks it), which is why range filtering
has a dedicated monomorphic entry point.

### 1.2 Module layout

```
crates/simd/src/
├── lib.rs            # re-exports, init(), calibration_report(), the dispatch-story module doc
├── dispatch.rs       # Isa, Source, forced_isa/with_forced_isa, the two env-var knobs, allows()
├── cpu.rs            # CPUID detect(), Vendor/CpuKey, Kernel, the known-CPU table (table_pick)
├── calibrate.rs      # micro-calibration: pick() with WARMUP=3, ITERS=7, MARGIN=5%
├── scalar.rs         # the scalar oracle for all seven primitives (correctness ground truth)
├── intersect.rs      # intersect + the GALLOP_RATIO=16 skew gate
├── lower_bound.rs    # the seek primitive (the measured SPB culprit)
├── merge.rs  dedup.rs  filter.rs  filter_indices.rs  gather.rs
```

## 2. Where SIMD plugs into the engine

The primitives are consumed by exactly two subsystems (the SPEC-12 F1/F2
consumers). Everything else — `merge`, `dedup`, `filter`/`filter_range` — is a
primitive with tests and benches but **no production consumer yet** (F3, the
delta-apply consumer, is unplanned and gated on issue #133).

### 2.1 WCOJ join executor (SPEC-03, F1)

| Call site | Primitive | Role |
|---|---|---|
| `wcoj/src/executor/wcoj.rs:517`, `trie/leapfrog.rs:131` | `intersect` | the `k == 2` leapfrog fast path: intersect two `active_run` slices in one bulk call instead of round-robin seeking |
| `wcoj/src/source/soa.rs:62` | `lower_bound` | seek within a SoA level column |
| `wcoj/src/source/packed_column.rs:188` | `lower_bound` | block-finish seek in a bit-packed column |

The leapfrog intersect path is documented in depth in
[`wcoj.md` §7](wcoj.md). Note the interplay: leapfrog feeds `intersect`
*skewed* runs (e.g. 3 keys vs 50 000), which is exactly what the skew gate (§4)
routes to the scalar gallop.

### 2.2 Storage: dictionary decode & partition scan (SPEC-02, F2)

| Call site | Primitive | Role |
|---|---|---|
| `storage/src/partition.rs:175` | `filter_indices_eq` | collect positions where a column equals a value (the `rdf:type` / object-equality partition scan) |
| `storage/src/partition.rs:177` | `gather` | map those positions back to subject ids |

Dictionary **inline-int decode** (`storage/src/dictionary.rs::decode_inline_ints`)
is SIMD-accelerated too, but via **autovectorisation** of a branchless scalar
loop, not a `horndb-simd` primitive — it is a pure arithmetic tag-check with no
gather/intersect shape, so the compiler vectorises it on its own. Keep this
distinction in mind: not all "SIMD-enabled" paths route through `horndb-simd`.

## 3. Kernel selection: the priority ladder

Each primitive resolves its kernel **once**, caching a
`(Isa, fn-pointer, Source)` triple in a `OnceLock` (`cached()` in each module),
so after the first call dispatch is a single load — there is no per-call
branching. `choose()` walks this ladder top to bottom:

1. **`forced_isa()`** (`dispatch.rs:59`) — a thread-local override set only by
   `with_forced_isa(...)`, used by `tests/differential.rs` and the benches to pin
   one exact ISA. Production never sets it; there is deliberately **no `Forced`
   `Source` variant**. Bypasses everything below (and, for `intersect`, the skew
   gate — see §4).

2. **`HORNDB_SIMD_MAX_ISA` cap** (`dispatch.rs:allows`) — a process-wide ceiling
   on the widest tier production may pick. Bounds the candidate list for every
   layer below. Details in §5.

3. **Known-CPU table** (`cpu.rs:table_pick`) — CPUID `(vendor, family, model)` →
   per-`Kernel` `Isa`, populated from **real SPB-256 measurements**. A hit picks a
   kernel with **no timing at all**, and records `Source::Table`. This is the
   authoritative layer: it exists precisely because microbench-driven calibration
   picked wrong on the hosts we run.

4. **Representative-input calibration** (`calibrate.rs:pick`, on by default) — the
   fallback for a CPU *not* in the table. Times every cap-allowed candidate on
   inputs shaped like the production access pattern (WARMUP=3 discarded, ITERS=7
   timed, keep the minimum) and adopts a SIMD kernel only if it beats scalar by
   `MARGIN` (5%). Records `Source::Calibrated`. Disabled with
   `HORNDB_SIMD_AUTOTUNE=off` (§5).

5. **Static widest-ISA preference** (calibration off) — pick the widest
   cap-allowed kernel the host supports, no timing. Records `Source::Static`.
   Scalar is always the baseline candidate, so this never fails.

### 3.1 The known-CPU table today

Both rows pin **every** kernel to scalar, each citing its SPB-256 basis
(`cpu.rs:table_pick`):

| Host | `(Vendor, family, model)` | Decision |
|---|---|---|
| AMD Zen4 (Ryzen 7 7700, hornbench) | `(Amd, 25, 97)` | all kernels → scalar (SPB: scalar 36.0 vs calibrated 28.6 qps) |
| Intel Sapphire Rapids (Xeon Gold 5412U, hel01) | `(Intel, 6, 143)` | all kernels → scalar (SPB: scalar 34.4 vs all-AVX2 17.3 qps) |

The table is keyed **per-`(cpu, kernel)`** on purpose: a future host that wins
with SIMD on some primitives but not others slots in without a signature change.
On non-x86-64 hosts (the aarch64 dev laptop) there is no accessible CPUID, so
`detect()` returns `None` and every primitive falls through to calibration.

**Adding a row** (full procedure in `crates/simd/AGENTS.md`): measure SPB-256
`aggregation-qps` on the host (on `hornbench`, never the laptop — see
[run benchmarks on hornbench]), A/B the candidate ISA against scalar, add a match
arm in `table_pick` citing the measurement, then sync `BENCHMARKS.md`,
`docs/architecture.md`, and `TASKS.md`.

## 4. The `intersect` skew gate

`intersect` is the one primitive with a **production-only** kernel switch that
sits *outside* the selection ladder. It picks between two asymptotically
different algorithms by input shape (`intersect.rs`):

- **Skewed** inputs (`max(|a|,|b|) / min(|a|,|b|) ≥ GALLOP_RATIO`, = 16) → scalar
  **galloping**, `O(|small|·log|large|)`.
- **Balanced** inputs → the **block-SIMD** kernel chosen by the ladder above,
  `O(|large|)`.

This matters because leapfrog feeds `intersect` skewed runs as the *common* case
(3 keys vs 50 000), where block-SIMD is 50–2000× slower. Two rules follow, and
both are load-bearing (do not "fix" them):

- **Benches MUST include skewed shapes.** A balanced-only bench once
  false-greened a −7% SPB regression. Skew coverage lives in
  `benches/intersect.rs`; correctness in `tests/skew_intersect.rs`.
- **Calibration MUST stay balanced-only.** Calibration picks the fastest *block*
  kernel and bypasses the gate; post-gate, the block kernel only ever runs on
  balanced inputs, so calibrating it on skew would optimise for data it never
  sees. The skew path is the uncalibrated scalar gallop — there is no kernel to
  choose there.

A `forced_isa` override routes straight to the forced block kernel and skips the
gate, so `tests/differential.rs` still exercises the block kernels while
`tests/skew_intersect.rs` (unforced) covers the gallop path.

## 5. Controlling selection with environment variables

There are exactly **two** environment variables. Both are read **once** (memoised
in a `OnceLock`) on first use, so set them before the process starts. Neither
affects the `with_forced_isa` test override, so the differential suite still
exercises every kernel the host can run regardless of what the shell sets.

### `HORNDB_SIMD_MAX_ISA` — the operational cap

A width **tier** ceiling on what production detection may pick:
`scalar < {avx2, neon} < avx512`. It bounds the candidate set for the table,
calibration, and static layers alike.

| Value(s) (case-insensitive, trimmed) | Effect |
|---|---|
| `scalar`, `none`, `off` | disable **all** SIMD — the escape hatch for isolating a suspected kernel regression in production |
| `avx2` | allow AVX2/NEON, **suppress AVX-512** — e.g. if Zen4 AVX-512 downclocking loses net on your workload |
| `avx512`, `avx512f`, `avx-512` | allow up to AVX-512 (the default ceiling on a capable host) |
| `neon` | allow up to NEON (aarch64) |
| unset / unrecognised | no cap |

Query the effective value at runtime with `configured_max_isa()`.

### `HORNDB_SIMD_AUTOTUNE` — the calibration toggle

Controls the calibration layer (step 4). **On by default.**

| Value | Effect |
|---|---|
| `off`, `0`, `false`, `no` (case-insensitive) | disable calibration → fall back to the **static widest-ISA** preference |
| unset / anything else | calibration enabled |

The `HORNDB_SIMD_MAX_ISA` cap still applies when calibration is off. Query with
`configured_autotune()`.

> **Interaction:** the known-CPU table (step 3) runs *before* calibration and is
> not disabled by `HORNDB_SIMD_AUTOTUNE=off`. On a listed CPU the table decides;
> the toggle only affects the fallback for *unlisted* CPUs. The cap bounds every
> layer in every mode.

## 6. Correctness: one oracle, and a cross-arch trap

Every SIMD kernel is proven **bit-identical** to `src/scalar.rs` by
`tests/differential.rs`, which forces each ISA via `with_forced_isa` and compares
against the scalar result over proptest-random and boundary inputs.

**⚠️ The cross-arch false-green.** CI runs on x86-64 (`ubuntu-latest`); the dev
laptop is aarch64. `tests/differential.rs` builds its forced-ISA list from
`is_x86_feature_detected!` / `is_aarch64_feature_detected!`, so **on the laptop
it only exercises Scalar + NEON** — the AVX2/AVX-512 arms never run. A bug in an
x86 kernel (the canonical example: a `dedup` AVX2 `u64::MAX` wrapping overflow)
passes locally and fails **only on Intel CI**. Before claiming any x86 kernel
correct:

- A local green is **not** proof — the x86 arms didn't run.
- Add boundary values (`0`, `u64::MAX`, empty / one-element slices) as explicit
  differential cases, not just proptest-random.
- A CI x86 failure here is **signal, not flakiness**.

## 7. Observability

At server startup, `serve.rs::record_simd_calibration` calls `horndb_simd::init()`
(priming every primitive) and publishes `horndb_simd::calibration_report()` —
one `(Kernel, Isa, Source)` per primitive — as the `horndb_simd_kernel_isa`
gauge, `1` on the chosen series. The `source` label (`table` / `calibrated` /
`static`) tells fleet ops *why* an ISA was picked — e.g. spotting hosts that fell
through to calibration because they're absent from the known-CPU table. The same
triple is logged at startup. See `docs/metrics.md` (the `horndb_simd_kernel_isa`
row) for the full label set.

## 8. Files & tests

| File | Role |
|---|---|
| `src/lib.rs` | re-exports, `init()`, `calibration_report()`, the canonical dispatch-story module doc |
| `src/dispatch.rs` | `Isa` / `Source`, `with_forced_isa`, the two env-var knobs, `allows()` cap check |
| `src/cpu.rs` | CPUID `detect()`, `Kernel`, the known-CPU `table_pick` |
| `src/calibrate.rs` | `pick()` — WARMUP=3, ITERS=7, 5% margin |
| `src/scalar.rs` | the scalar oracle (correctness ground truth) |
| `src/intersect.rs` | `intersect` + the skew gate |
| `tests/differential.rs` | per-ISA bit-identity proptest (the correctness gate) |
| `tests/skew_intersect.rs` | unforced coverage of the gallop path |
| `tests/env_cap.rs`, `tests/calibration*.rs` | the env-var knobs |
| `benches/intersect.rs` | `intersect` throughput (record on `hornbench`; must include skew) |

```bash
cargo nextest run -p horndb-simd            # scalar + host-native ISA only (see the cross-arch trap)
cargo bench -p horndb-simd --bench intersect  # record on hornbench, never the laptop
```

## 9. Deferred / out of scope

- **F3 delta-apply SIMD** (`merge`/`dedup` consumer) — unplanned, gated on issue
  #133. The `merge`/`dedup`/`filter*` primitives exist but have no production
  consumer today.
- **SIMD for the `cax-sco` / `rdf:type` materialization scan** — explicitly out
  of scope: profiling (#133) showed that hotspot is fixed by *indexing*, not
  vectorisation.
- **Per-kernel SIMD table rows** — the table shape supports selecting different
  ISAs per primitive per host; today every row is uniformly scalar.

## References

- `docs/specs/SPEC-12-simd.md` — the subsystem contract and acceptance criteria.
- `crates/simd/AGENTS.md` — contributor gotchas: the cross-arch false-green, the
  known-CPU-table-first rule, the skew-gate rules, and the row-addition procedure.
- `crates/simd/simd-research-findings.md` — the AMD/Intel AVX-512 research behind
  the table (why `/proc/cpuinfo` `avx512*` flags aren't a throughput proxy).
- [`wcoj.md`](wcoj.md) §7 — the leapfrog `k == 2` intersect consumer in depth.
- `docs/metrics.md` — the `horndb_simd_kernel_isa` metric surface.
