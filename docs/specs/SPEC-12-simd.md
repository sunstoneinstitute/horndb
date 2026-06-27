# SPEC-12 — SIMD Acceleration Layer

## Purpose

Define a single, shared, runtime-dispatched SIMD layer for HornDB's data-parallel
hot loops, and the contract every consumer of it obeys. The motivating gap is
SPEC-03 NF1: the WCOJ per-tuple overhead target is ≤5 ns/tuple against DuckDB's
~2 ns/tuple, and the Stage-1 executor is explicitly allowed to miss it because it
is "binary-search-based seek, no SIMD yet" (`BENCHMARKS.md`, *Scaffolded* table,
`benches/per_tuple.rs`). This SPEC is the contract for closing that gap — and the
analogous gaps in dictionary decode and the columnar `rdf:type` partition scan —
with **explicit intrinsics behind runtime CPU feature detection**, not with
autovectorization hope and not by leaving each crate to hand-roll its own.

The bet (SPEC-00 bet 2: unified-memory hardware as a first-class target) only pays
off if the inner loops saturate memory bandwidth and the per-tuple ALU work is
vectorized; a scalar inner loop wastes both HBM bandwidth and the wide vector
units on the EPYC Zen4 reference host. SIMD is the lever for the loops that are
already *algorithmically right* — it is **not** a substitute for the missing
indexes and semi-naïve firing that dominate the SPEC-04 materialize path (see
Scope / non-goals and the §F3 caveat).

## Scope

The layer is a **cross-cutting primitives module** plus four named consumer hot
paths, ranked by payoff (highest first):

1. **WCOJ leapfrog intersect + seek** (SPEC-03, F1) — the classic, highest-payoff
   SIMD win: vectorized sorted-set intersection and a galloping/SIMD lower-bound
   seek replacing the current branchy binary search.
2. **Dictionary encode/decode + columnar partition scans** (SPEC-02, F2) —
   including the ≥80% STREAM-Triad `rdf:type` partition scan (SPEC-02 NF2,
   acceptance #4).
3. **Delta-apply merge / dedup / sort** of derived triples (SPEC-04, F3) —
   **gated behind issue [#2](https://github.com/sunstoneinstitute/horndb/issues/2)**;
   see the caveat below.
4. **The shared SIMD primitives layer itself** (F4) — `intersect`, `merge`,
   `dedup`, `filter`, `gather`, `lower_bound` over `&[u32]`/`&[u64]` slices,
   consumed by 1–3 rather than duplicated per crate.

In scope:
- A new leaf crate **`horndb-simd`** (recommendation, see F4 + Dependencies) holding
  the primitives, their per-ISA implementations, and the scalar oracle.
- Runtime ISA dispatch: AVX2 and AVX-512 on x86-64 (Zen4 reference host), NEON on
  aarch64 (Apple-Silicon dev Macs), **scalar fallback always present and always the
  correctness oracle**.
- `std::arch` intrinsics on **stable Rust 1.90** only.
- The differential-correctness obligation: every SIMD kernel is proven
  bit-identical to its scalar oracle by a proptest, in the style already used for
  the WCOJ binary-join fuzzer (SPEC-03 acceptance #3) and the owlrl
  closure-backend differential (`crates/owlrl/tests/closure_backend_differential.rs`).

Out of scope (non-goals):
- **Nightly Rust, `std::simd` / `portable_simd`, or any other unstable feature.**
  The workspace is pinned to stable 1.90 (`rust-toolchain.toml`); this is a hard
  constraint, not a preference.
- **Relying on the compiler's autovectorization as the contract.** Today's
  `VecIter::seek` leans on `partition_point` auto-vectorizing
  (`crates/wcoj/src/source/vec_source.rs:106-108`); that is a happy accident the
  optimizer can withdraw between toolchain bumps. Where this SPEC names a target,
  the kernel is explicit intrinsics with a measured floor, so the number cannot
  silently regress. Autovectorized scalar code may remain as the *fallback*, but
  never as the load-bearing path for a gated NF.
- **SIMD as a replacement for an algorithmic fix.** The ~480 ms cax-sco /
  `rdf:type` materialization hotspot is an **un-indexed O(N) full-partition scan**
  (`crates/owlrl/src/store.rs:170-184`: `probe(None, rdf_type, Some(c1))` filters
  the entire predicate partition; the `_delta` argument is unused, so semi-naïve is
  effectively naïve and the scan repeats per subclass-pair and per round). The
  fix is an **object index** on the `rdf:type` partition and **genuine
  delta-driven semi-naïve firing** — issue
  [#2](https://github.com/sunstoneinstitute/horndb/issues/2), SPEC-04 F5 — **not**
  SIMD. Vectorizing that scan would optimize a loop that indexing *deletes*. This
  SPEC explicitly does **not** scope SIMD onto the cax-sco partition filter.
- GPU / SVE / wider-than-512 ISAs — SPEC-09, Stage 3.
- Changing the on-disk or Arrow exchange formats. SIMD operates on the existing
  buffers in place (SPEC-03 NF2).

## Functional requirements

**F1. WCOJ leapfrog intersect + seek (SPEC-03 hot path).**
- **Seek.** Replace the scalar lower-bound in the trie cursors with a
  `horndb-simd` `lower_bound` that gallops (exponential probe) and then finishes
  with a SIMD block compare, over the existing sorted columns:
  `VecIter::seek` (`crates/wcoj/src/source/vec_source.rs:98-120`),
  `CompressedIter::seek` (`crates/wcoj/src/source/compressed.rs:116-121`), and
  `PackedColumn::lower_bound` (`crates/wcoj/src/source/packed_column.rs:132-143`).
- **Intersect.** The leapfrog round-robin advance
  (`LeapfrogJoin::find_match`, `crates/wcoj/src/trie/leapfrog.rs:101-117`) gains a
  fast path: when two cursors are at the same trie level over contiguous sorted
  runs, intersect them with a vectorized sorted-set intersection
  (`simd::intersect`) instead of one-at-a-time `seek`/`peek`. The k-way leapfrog
  invariant (running max via the `order` permutation) is preserved; the SIMD path
  is a pairwise accelerator inside it, not a rewrite of the algorithm.
- Output bindings remain bit-identical to the scalar leapfrog (SPEC-03 NF4).
- Note: the dense trie store is currently AoS `Vec<(u64,u64,u64)>`
  (`vec_source.rs`); a column-major (SoA) view of the active level is a
  prerequisite for an efficient intersect and is part of this work.

**F2. Dictionary + columnar scan (SPEC-02 hot path).**
- **Decode.** Vectorize the bulk `TermId → Term` path used by scans and result
  materialization, including the inline-integer fast path
  (`crates/storage/src/dictionary.rs:78-84`,
  `crates/storage/src/term.rs:68-74`): a batch of inline-int `TermId`s decodes with
  a SIMD unpack/bitcast rather than per-element.
- **Partition scan.** The predicate-partition scan
  (`crates/storage/src/partition.rs:81-88`, `:201-220`) gains a vectorized
  gather/filter path so that the `rdf:type` sequential scan reaches the SPEC-02 NF2
  ≥80% STREAM-Triad bandwidth target. This is the SIMD-friendly half of the
  SPEC-02 acceptance #4 work (the other half is NUMA-pinned bench hardware,
  already deferred to Stage 2 in the SPEC-02 plan).
- **Encode.** Vectorized membership/scan inside `Dictionary::intern`'s hot path is
  in scope only as a stretch; the interner's DashMap+reverse-vec shape
  (`dictionary.rs:35-45`) is not naturally SIMD-friendly and is lower priority than
  decode.

**F3. Delta-apply merge / dedup / sort (SPEC-04 hot path) — GATED.**
- This requirement is **blocked on and lower priority than** issue
  [#2](https://github.com/sunstoneinstitute/horndb/issues/2). SIMD here targets the
  parts that remain after indexing lands, **not** the cax-sco scan.
- After #2, the SIMD-friendly residue is: (a) the **delta-apply merge/dedup/sort**
  of derived triples — today `Delta` is unordered `FxHashSet`/`FxHashMap`
  (`crates/owlrl/src/delta.rs:9-10`, with `merge`/`insert`/`subtract` at `:18-73`
  doing hash-based dedup), so this F **requires** first representing the delta as
  sorted columnar runs, then merging/deduping them with `simd::merge` +
  `simd::dedup`; and (b) **vectorized membership filters over contiguous class
  extents** — the per-subject `store.contains(...)` checks in the list rules
  (`crates/owlrl/src/list_rules.rs:769-773`, `:896-899`) become a vectorized
  semijoin against a sorted extent once the extent is materialized contiguously.
- The representational change (hash-delta → sorted-run delta) is the bulk of this
  work; the SIMD kernels are reused from F4. If #2's indexing makes the hash delta
  cheap enough, this F may be descoped — decide after #2 measures.

**F4. Shared SIMD primitives layer.**
- A single module — recommended as a new **leaf crate `horndb-simd`** with zero
  HornDB dependencies — exporting safe wrappers over: `lower_bound` (galloping +
  SIMD block compare), `intersect` (sorted-set), `merge` (sorted two-way),
  `dedup` (sorted run), `filter` (predicate mask → compacted output), and `gather`
  (indexed load). All operate on primitive slices (`&[u32]`, `&[u64]`), never on
  HornDB domain types, so the crate stays a dependency-free leaf.
- **Dispatch.** Each primitive selects its implementation **once** via a cached
  function pointer resolved at first use from `is_x86_feature_detected!("avx512f")`
  / `is_x86_feature_detected!("avx2")` /
  `std::arch::is_aarch64_feature_detected!("neon")`, falling back to scalar. No
  per-call feature detection in the hot loop.
- **`#[target_feature]` / unsafe discipline.** Each ISA kernel is an
  `#[target_feature(enable = "…")]` `unsafe fn`; the `unsafe` is confined to the
  kernel body and the one dispatch site that has proven the feature is present.
  The public wrapper is safe. This crate is the **only** place in the workspace
  allowed to carry hand-written SIMD intrinsics (storage/wcoj/owlrl carry none
  today — confirmed by the survey behind this SPEC); consumers call the safe
  wrappers.
- **Scalar oracle.** The scalar implementation of every primitive is always
  compiled and is the reference for the differential proptests (NF3); it is what
  runs on any ISA without a matching kernel.

**F5. Feature-detection + dispatch is testable.** A test-only override forces the
scalar / AVX2 / AVX-512 / NEON path regardless of the host CPU (env var or
cfg-gated setter), so CI can exercise every kernel that the host *can* execute and
the differential tests cover each path, not just whichever the runner happened to
pick.

## Non-functional requirements

**NF1. WCOJ per-tuple overhead.** Close the SPEC-03 NF1 gap: with F1 landed,
`benches/per_tuple.rs` reaches **≤2.5 ns/tuple** on the reference workstation (from
the ≤5 ns Stage-1 envelope toward DuckDB's ~2 ns), measured on hornbench. The
existing 4-cycle gate (`benches/four_cycle.rs`, ≥10× binary-hash, currently ~34×)
must not regress.

**NF2. Sorted-set intersection throughput.** The `simd::intersect` primitive
sustains a vectorized-vs-scalar speedup ≥**4×** on the AVX-512 reference host (≥2×
on NEON) on a microbench over sorted `u32`/`u64` runs at L2-resident sizes — the
floor that justifies the intrinsics maintenance cost.

**NF3. Correctness — differential vs scalar oracle.** Every SIMD kernel is
bit-identical to its scalar oracle for all inputs, proven by a proptest per
primitive (random lengths, overlaps, duplicates, boundary/empty cases). This is a
hard gate: a kernel ships only with its differential test green on every path the
CI host can execute (NF / F5). Mirrors the WCOJ binary-join fuzzer and the
closure-backend differential already in the tree.

**NF4. Dictionary decode throughput.** Bulk inline-int decode (F2) sustains
≥**4×** scalar on the reference host on a decode microbench; the `rdf:type`
partition scan (F2) reaches ≥**80% STREAM-Triad** bandwidth (SPEC-02 NF2 floor),
measured on the NUMA-pinned hornbench bench.

**NF5. Portability / dispatch correctness.** A single source tree builds and passes
on x86-64 (AVX2 + AVX-512) and aarch64 (NEON) with **no nightly features**; on a
host lacking every accelerated ISA, the scalar path runs and all tests pass.
Dispatch cost is amortized (cached pointer), adding no measurable per-call
overhead to the scalar baseline.

## Dependencies

- **SPEC-02 (storage)** — dictionary, `TermId`, columnar partitions; the F2
  consumer and the lowest layer that depends on `horndb-simd`.
- **SPEC-03 (wcoj)** — the F1 consumer; the highest-payoff path.
- **SPEC-04 (owlrl)** — the F3 consumer, **gated on issue
  [#2](https://github.com/sunstoneinstitute/horndb/issues/2)**.
- **SPEC-01 (harness / BENCHMARKS.md)** — the per_tuple / intersect / decode /
  partition-scan benches that gate this SPEC.
- No new external crates: `std::arch` intrinsics only.

**Dependency-order implication.** The primitives crate must sit **below** every
consumer. With the existing order `storage → wcoj → {owlrl, closure} → incremental
→ sparql`, `horndb-simd` becomes a new zero-dependency leaf *under* `storage`:
`simd → storage → wcoj → {owlrl, closure} → …`. A new leaf crate is preferred over
a `horndb-storage::simd` module because (a) the primitives are pure slice
operations with no storage types, so they belong below storage, not inside it;
(b) it confines the entire `unsafe`/`#[target_feature]` surface to one auditable,
independently-testable crate; and (c) it avoids any temptation toward a cycle
(owlrl's delta-apply needs the primitives but must not pull in storage internals
to get them).

## Acceptance criteria

Honoring the harness-first rule (SPEC-00): each criterion gates on a named
criterion/harness bench, every recorded number measured on hornbench and written to
`BENCHMARKS.md`.

1. **Primitives differential.** A proptest suite in `horndb-simd` proves every
   primitive (`lower_bound`, `intersect`, `merge`, `dedup`, `filter`, `gather`)
   bit-identical to its scalar oracle on scalar **and** every ISA path the CI host
   can execute (forced via F5). Zero mismatches. (NF3)
2. **WCOJ per-tuple.** `benches/per_tuple.rs` (replacing the current stub) is wired
   as a real microbench and reaches **≤2.5 ns/tuple** on hornbench; `four_cycle.rs`
   stays ≥10× (no regression). New/updated `BENCHMARKS.md` row: `per_tuple`. (NF1)
3. **Intersection throughput.** A new `benches/intersect.rs` (in `horndb-wcoj` or
   `horndb-simd`) shows ≥4× SIMD-over-scalar on AVX-512 / ≥2× on NEON, recorded in
   `BENCHMARKS.md`. (NF2)
4. **Dictionary decode + partition scan.** A new dictionary-decode microbench shows
   ≥4× scalar; the `rdf:type` partition scan reaches ≥80% STREAM-Triad on the
   NUMA-pinned hornbench bench (jointly satisfies SPEC-02 acceptance #4). (NF4)
5. **WCOJ correctness preserved.** The SPEC-03 differential fuzzer
   (`crates/wcoj/tests/differential_fuzz.rs`) stays green with the SIMD seek/intersect
   path enabled — bindings bit-identical to binary-join.
6. **No nightly, portable build.** The workspace builds and tests green on stable
   1.90 on x86-64 and aarch64; a scalar-forced run (F5) passes with every SIMD path
   disabled. (NF5)
7. **Delta-apply SIMD (gated).** *Only after issue
   [#2](https://github.com/sunstoneinstitute/horndb/issues/2) lands:* a delta-apply
   merge/dedup microbench over sorted-run deltas shows a measured win over the
   hash-delta baseline, differential-proven equal to the current `Delta` semantics.
   This criterion is **deferred** until #2 and may be descoped if #2 makes it
   irrelevant.

## Roadmap / staging

1. **First: `horndb-simd` (F4) + WCOJ (F1).** The primitives crate with its scalar
   oracle and differential proptests lands first (it gates everything), immediately
   followed by the WCOJ seek/intersect consumer — the highest-payoff path and the
   one with the clearest gated NF (per_tuple). Acceptance #1, #2, #3, #5.
2. **Second: dictionary decode + partition scan (F2).** Reuses the same primitives;
   pairs naturally with the SPEC-02 NUMA-pinned STREAM bench. Acceptance #4.
3. **Last, gated: delta-apply SIMD (F3).** Blocked on issue
   [#2](https://github.com/sunstoneinstitute/horndb/issues/2) (object index +
   semi-naïve). Requires the hash-delta → sorted-run representational change before
   any kernel helps. Acceptance #7. The cax-sco partition-filter scan is **out of
   scope** — superseded by #2's indexing.

## Risks and open questions

- **Intrinsics maintenance cost.** Three ISA variants per primitive plus a scalar
  oracle is real surface. Mitigation: confine all of it to one leaf crate, keep the
  primitive set small (six), and let the differential proptest (NF3) catch drift.
  A primitive earns its intrinsics only if it clears the NF2/NF4 ≥4× floor;
  otherwise ship scalar.
- **AVX-512 downclocking on Zen4.** Wide AVX-512 can trigger frequency reduction;
  on some workloads a 256-bit (AVX2 / "AVX-512 on 256-bit") path wins net. Zen4's
  double-pumped 256-bit AVX-512 datapath largely avoids the worst Intel-style
  licence-based downclocking, but this must be **measured**, not assumed — the
  bench compares AVX2 and AVX-512 kernels on hornbench and dispatch picks the
  faster, which may be AVX2 for some primitives.
- **NEON ↔ AVX divergence.** Two hand-written vector paths can diverge in edge
  cases (tail handling, overflow). The per-path differential test (F5/NF3) is the
  guard; CI must exercise every path the host supports, not just the default.
- **Unsafe surface.** `#[target_feature]` `unsafe fn`s are a soundness obligation:
  calling one without the feature present is UB. Mitigation: a single dispatch site
  per primitive that has proven the feature, safe public wrappers, and the scalar
  oracle as the only always-callable path.
- **The differential-correctness obligation is non-negotiable.** A SIMD kernel that
  is fast but not bit-identical to the scalar oracle is a correctness regression in
  a *reasoner* — every materialized triple must be sound (SPEC-00 bet 6). No kernel
  ships without its proptest green.
- **F3 may evaporate.** If issue #2's indexing + semi-naïve makes the delta cheap
  enough, the SIMD delta-apply may not be worth the representational change. This is
  an explicit "measure after #2" decision, not a commitment.
- **Open: AoS → SoA for the WCOJ trie.** F1's intersect wants column-major active
  levels; the current dense store is AoS tuples. Whether to keep a transient SoA
  view per level or change the trie storage layout is a SPEC-03 design decision this
  SPEC surfaces but does not settle.
