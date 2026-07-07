# Benchmarks

Where we are, where we need to be, and what we measure against. This document
is the single source of truth for the project's quantitative goals: targets
come from the per-subsystem SPECs (non-functional requirements and acceptance
criteria), baselines from the cited literature and vendor publications, and
the *Current results* section records the measured state. Live gaps are
tracked in [`../TASKS.md`](../TASKS.md).

## Reference hardware

The "reference workstation" referenced throughout the SPECs:

- **CPU:** single AMD EPYC 9354 (Zen 4, 32C/64T)
- **DRAM:** 12-channel DDR5-4800
- **Storage:** local NVMe (HDT cold tier; SPEC-02)
- **Stage 3 only — accelerators:** AMD MI300A (preferred for unified HBM + Zen4) or NVIDIA GH200 / GB200

The harness captures a hardware fingerprint per run; comparisons are valid
only within identical fingerprints (SPEC-01 NF — "we normalise by capturing
the fingerprint, not by trying to normalise across hardware").

**Where benchmarks are run.** All `cargo bench` runs that produce numbers
recorded in this document are executed on the dedicated **`hornbench`** server
(`ssh hornbench`; repo at `~/src/horndb`), *not* on a laptop — this keeps the
environment stable and comparable over time. Check out the commit under test
on hornbench (or `rsync` over uncommitted files), run the bench there, and
record the numbers with their environment. Numbers measured on a laptop are
provisional and must be re-baselined on hornbench before being treated as
authoritative. A second x86 host, **`hel01`** (Intel Xeon Gold 5412U,
Sapphire Rapids), serves as the Intel counterpoint for ISA-sensitive work.

## Baselines we measure against

| Engine | Role | Source of numbers |
|---|---|---|
| **RDFox** (Samsung / Oxford) | Materialization throughput leader. Pure forward-chaining. SPB-256 A/B driver: `../crates/harness/scripts/run-rdfox-spb-256.sh` (requires a benchmarking license — see DeWitt note below). | ISWC 2015 paper: **6.1 M triples/sec** materialization on SPARC T5-8 (128 cores, 4 TB RAM). RDFox's own statement: pure-materialization gives up **100–1000×** on backward chaining. |
| **GraphDB Enterprise** (Graphwise) | SPARQL throughput leader. Java/RDF4J derived. | LDBC SPB published baseline: expansion ratio **1:3.2** on SPB-256 OWL 2 RL run. |
| **GraphDB Free** | Open competitor accessible without procurement. | Our differential A/B reference for nightly LDBC SPB-256 (`../crates/harness/scripts/bootstrap-graphdb-free-spb.sh`). |
| **Oxigraph** (Rust, RocksDB) | Closest architectural peer: Rust SPARQL 1.1 store, no reasoner — serves the same flat closure as HornDB. MIT/Apache-2.0, so numbers publish freely (no DeWitt clause). | Nightly LDBC SPB-256 A/B reference (`../crates/harness/scripts/bootstrap-oxigraph-spb.sh`); pinned via `OXIGRAPH_VERSION` in `nightly.yml`. |
| **Inferray** | Transitivity-closure speed record on commodity hardware. | **21.3 M triples/sec** closure on a single Intel desktop; **142×** vs RDFox and **590×** vs GraphDB/OWLIM on transitivity-chain. |
| **Apache Jena (+ WCOJ extension)** | WCOJ reference point. | Hogan et al. ISWC '19: **1–2 orders of magnitude** speedup over baseline Jena on WatDiv shapes. |
| **DuckDB** | Per-tuple-overhead reference. | Published baseline ~**2 ns/tuple** for simpler operators. |

A note on **publication of comparative numbers**: RDFox commercial licenses
typically forbid published comparative benchmarks (the so-called "DeWitt
clause"). Internal use against an RDFox benchmarking license is permitted and
is the Stage-1 expectation; publishing requires legal review (SPEC-01 Risks).
GraphDB Free has no such restriction.

## Stage gates

These are the project-level go/no-go thresholds from
[`specs/SPEC-00-vision.md`](specs/SPEC-00-vision.md).

| Stage | Workload | Target | Stop-the-line if |
|---|---|---|---|
| **Stage 1** (feasibility prototype) | LUBM-100 materialization | within **3×** of RDFox | red on selected W3C subset (≥50 cases) |
| **Stage 1** | Selected W3C OWL 2 RL subset | **100%** green | any case red |
| **Stage 2** (MVP) | LUBM-8000 materialization | within **2×** of RDFox | red on full W3C OWL 2 RL + SPARQL 1.1 + Entailment Regimes |
| **Stage 2** | LDBC SPB SF3 read | ≥**50%** of GraphDB Enterprise throughput | ORE 2015 OWL 2 RL fragment <100% solved |
| **Stage 3** (hardware specialization) — *win condition* | LDBC SPB SF5 (~1B edges) on a single MI300A or GH200 | ≥**1.5×** RDFox materialization **and** ≥**2×** GraphDB Enterprise query throughput | "Stage 3 has not earned its budget" — SPEC-09 NF5 |

> **Stage-1 LUBM gate status:** wired and runnable via
> `../scripts/bench/compare-rdfox.sh --lubm N` (identical TBox+ABox and rule
> set through both engines, closure-count parity gate + wall-clock cap). The
> parity gate passes exactly (delta 0,
> [#59](https://github.com/sunstoneinstitute/horndb/issues/59)). The 3×
> *timing* gate is still open and is **not** closure-bound: per-phase
> profiling ([#61](https://github.com/sunstoneinstitute/horndb/issues/61))
> attributes the LUBM-shaped materialize cost to the compiled `cax-sco`
> type-expansion + delta apply (closure ≈0.3% of reason time), which is the
> object-index + semi-naïve work in
> [#133](https://github.com/sunstoneinstitute/horndb/issues/133). LUBM-100
> (the literal gate) has not run yet — generation needs Jena `riot`. RDFox
> comparison numbers are internal-only (DeWitt clause) and are never recorded
> here.

## Per-subsystem targets (Stage 2 unless noted)

Numbers below are pulled directly from each SPEC's NF section and acceptance
criteria. They are the floor each subsystem must hit before it's "done."

### SPEC-02 — Storage (`horndb-storage`)

| Metric | Target | Baseline |
|---|---|---|
| Bulk N-Triples import | ≥**1 M triples/sec** | RDFox (F8) |
| LUBM-100 bulk-import (~13 M triples) | ≤**30 s** on reference workstation | acceptance #1 |
| LUBM-8000 bulk-import (~1.1B triples) | ≤**30 minutes** on reference workstation | acceptance #2 |
| Warm-tier memory footprint | ≤**50 bytes/triple** | RDFox: 36.9 (NF1; we accept ~35% headroom for all 6 orderings) |
| Cold-tier (HDT) footprint | ≤**6 bytes/triple** amortised | NF1; measured **5.440 B/triple** on a 40k-triple synthetic LUBM-shaped corpus (`snapshot/`, SPEC-02 F9) — synthetic, validate against real LUBM |
| LUBM-8000 warm footprint | ≤**55 GB** | acceptance #3 |
| `rdf:type` partition scan throughput | ≥**80% of STREAM Triad** bandwidth | NF2, acceptance #4 |
| Tiering write amplification | ≤**1 rewrite/tier**, ≤**2× read amp** from cold | NF4 |

### SPEC-03 — WCOJ query engine (`horndb-wcoj`)

| Metric | Target | Baseline |
|---|---|---|
| Per-tuple overhead (hot path) | ≤**5 ns/tuple** | DuckDB ~2 ns/tuple (NF1, 2.5× envelope for the trie machinery) |
| Parallel scaling | ≥**0.7 × N** on N cores | NF3 |
| Cancellation latency | ≤**100 ms** | NF (acceptance #5) |
| **4-cycle on 10⁶-edge synthetic graph** | WCOJ ≥**10×** binary-hash join | canonical WCOJ-wins case (acceptance #2) |
| WatDiv SF100 query latency | within **2×** of Jena+WCOJ | Hogan et al. (acceptance #1) |
| Magic-sets `subClassOf+` over SNOMED CT | ≤**2×** materialized-scan wall time | acceptance #4 |
| Differential fuzzer (100K random BGPs over LUBM-100) | **zero** mismatches vs binary-join | acceptance #3 |

### SPEC-04 — OWL 2 RL rule engine (`horndb-owlrl`)

| Metric | Target | Baseline |
|---|---|---|
| LUBM-8000 materialization throughput | ≥**2 M triples/sec** | RDFox 6.1 M on much larger hardware (NF1, ~1/3 ratio) |
| LUBM-8000 full materialization wall time | ≤**10 minutes** | acceptance #2 (implied ~1.8 M tps after subtracting GraphBLAS closure) |
| Expansion ratio (OWL 2 RL workloads) | ≤**4×** asserted | GraphDB 1:3.2 (NF2, acceptance #3) |
| Steady-state rule firing latency (LUBM-1000 warm store, single-triple insert) | ≤**1 s** | NF3 (jointly owned with SPEC-06) |
| Proof-tree retrieval (depth ≤10) | ≤**100 ms** | NF4 |
| `eq_rep_p_skew` — `eq-rep-p` class canonicalization (k=32 mutual-`owl:sameAs`, rows=8) | optimized path ≤ naive, identical closure (differential proptest) | measured **38.1 ms** optimized vs **48.7 ms** naive (~1.28×); output blow-up is semantically irreducible |
| `rdf_type_skew` — F5 `rdf:type` partition-by-class parallelism ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)) | parallel (`Auto`) ≤ serial, **identical** closure (`tests/rdf_type_skew_differential.rs`) | measured (macOS dev workstation, 2026-06-18): 100 k subjects **172.6 ms** `Auto` vs **199.6 ms** `Serial` (~1.16× over the whole `materialize`; the rule-local speedup is larger) |

### SPEC-05 — GraphBLAS closure backend (`horndb-closure`)

| Metric | Target | Baseline |
|---|---|---|
| Transitive closure (25K-node Inferray-shape chain) | ≥**10 M triples/sec** | Inferray 21.3 M (NF1; we pay for GraphBLAS generality) |
| Transitivity-chain (2,500 nodes) | ≥**10×** RDFox, ≥**50×** GraphDB/OWLIM | Inferray 142× / 590× (acceptance #1, looser to absorb integration overhead) |
| LUBM-8000 closure memory | ≤**2×** original transitive-property triples | NF3 / acceptance #5 |
| Closure vs SPEC-04 rule-firing (LUBM-100) | **identical** triple set | acceptance #4 |
| Routing heuristic | SPEC-04 if `nnz(M_p) < 10⁴`, else SPEC-05 | Risks — threshold needs bench tuning |
| Incremental single-edge insert vs full recompute (F6, 2,000-node chain) | incremental ≪ full recompute | `benches/incremental.rs` — see *Measured* below |
| Valued-reasoning readiness ([#11](https://github.com/sunstoneinstitute/horndb/issues/11)) — valued `(max,×)` vs boolean `(∨,∧)` closure; generic-kernel penalty | _instrument, then decide_ | `benches/valued_readiness.rs` — see *Measured* below |
| Valued best-confidence crosswalk closure ([#12](https://github.com/sunstoneinstitute/horndb/issues/12) Fork A) | one `(max,×)` closure replaces a SPARQL property-path crawl | `benches/crosswalk.rs` — see *Measured* below |

### SPEC-06 — DBSP incremental maintenance (`horndb-incremental`)

| Metric | Target | Baseline |
|---|---|---|
| Steady-state insert/retract latency (LUBM-1000 warm) | ≤**100 ms** | NF1 / acceptance #1 (jointly owned with SPEC-04 NF3) |
| Sustained insert throughput (warm LUBM-8000) | ≥**100K triples/sec** | NF2 / acceptance #2 |
| Query-latency degradation under sustained write load | ≤**2×** no-write baseline | acceptance #2 |
| Pending delta size between checkpoints | ≤**5%** of main store | NF3 |

> **Stage 1 reality check:** NF1 and NF2 are *Stage-2 gates*. Stage-1 ships
> only the criterion benchmark scaffold (`benches/insert_throughput.rs`) on a
> synthetic 10K-triple fixture so regressions become visible as the real
> engine lands. Retraction is deferred entirely — see
> `../crates/incremental/FUTURE-WORK.md`.

### SPEC-07 — SPARQL 1.1 frontend (`horndb-sparql`)

| Metric | Target | Baseline |
|---|---|---|
| LDBC SPB SF3 geomean read latency | ≤**2×** GraphDB Enterprise | NF1 / acceptance #3 |
| Sustained simple-INSERT throughput (warm LUBM-8000 + SPEC-06 maintenance) | ≥**10K stmts/sec** | NF2 / acceptance #4 |
| Parser+planner throughput (SPB mix, no execution) | ≥**10K queries/sec** | NF3 |
| Concurrent in-flight queries | ≥**256** with sub-linear degradation | NF4 |
| Materialized vs backward-chained mode on LUBM-100 | **identical** result sets | acceptance #6 |

### SPEC-08 — ML/LLM integration (`horndb-ml`)

| Metric | Target | Baseline |
|---|---|---|
| Plan-advisor call overhead | ≤**1 ms** p99 (else planner skips + warns) | NF2 |
| Candidate-generator admission rate | ≥**10K candidates/sec** | NF3 |
| LLM endpoint engine-side overhead | ≤**50 ms** p99 (excludes upstream LLM API) | NF4 |
| Reference `CandidateGenerator` (FAISS, person ER) | ≥**10×** brute-force scan; symbolic re-verify rejects ≥**99%** of false positives | acceptance #2 |
| NL→SPARQL endpoint validity | ≥**80%** on a curated 100-question benchmark | acceptance #3 (Stage 2) |

### SPEC-09 — Hardware specialization (Stage 3)

| Metric | Target | Baseline |
|---|---|---|
| GPU GraphBLAS closure (100M-edge synthetic) | ≥**10×** CPU GraphBLAS | NF1 / acceptance #1 |
| GPU WCOJ (HBM-fit hot patterns) | ≥**5×** CPU WCOJ end-to-end | STMatch reports up to 3385× in kernel terms; 5× absorbs integration overhead (NF2) |
| CXL tier read latency | p99 ≤**500 ns** (Astera Labs Leo or equivalent) | NF3 |
| CXL tier-promotion (1 MB page) | ≤**10 ms** | NF3 |
| 4-node multi-node scale (LUBM-8000) | ≥**3×** single-node (≥75% efficiency) | NF4 / acceptance #4 |
| 8-node multi-node scale | ≥**5×** single-node (≥60% efficiency) | NF4 |
| **Stage 3 win condition** — LDBC SPB SF5 on single MI300A/GH200 | ≥**1.5×** RDFox materialization **and** ≥**2×** GraphDB Enterprise queries | NF5 / acceptance #5 |
| LUBM-8000 with 50% in CXL tier | within **1.3×** all-DDR5 baseline | acceptance #3 |

### SPEC-11 — SSSOM mappings & crosswalk index (`horndb-owlrl` + `horndb-storage`)

| Metric | Target | Baseline |
|---|---|---|
| Chain-rule closure throughput (SSSOM mappings) | **TBD** (NF1) | RDFox/Inferray closure leaders; measured: pending hornbench (F5/F6 follow-up) |
| Compact crosswalk-index footprint | ≤**10 bytes/pair** bidi (NF2, F5) | EF+FOR baseline → rung-4 PGM; measured: pending hornbench (F5/F6 follow-up) |
| Full-closure materialization vs OxO2 | beat **1.16 M mappings / 17 min** (NF3) | OxO2 (EBI Ontology Xref Service) reference run; measured: pending hornbench (F5/F6 follow-up) |

### SPEC-12 — SIMD acceleration layer (`horndb-simd`)

| Metric | Target | Baseline |
|---|---|---|
| WCOJ per-tuple overhead (`per_tuple`) | ≤**2.5 ns/tuple** | DuckDB ~2 ns; closes the SPEC-03 NF1 5 ns envelope (NF1) |
| Sorted-set intersection SIMD speedup (`intersect`) | ≥**4×** AVX-512 / ≥**2×** NEON vs scalar | NF2 |
| Bulk dictionary decode SIMD speedup | ≥**4×** scalar | NF4 |
| `rdf:type` partition scan | ≥**80% STREAM Triad** bandwidth | SPEC-02 NF2 / acceptance #4 (jointly owned) |
| Per-kernel differential vs scalar oracle | **zero** mismatches, every ISA path | NF3 |

> SIMD is scoped to the loops that are already *algorithmically right* — WCOJ
> seek/intersect, dictionary decode, columnar scans. It is **not** the lever
> for the `cax-sco` / `rdf:type` materialization hotspot, which is an
> un-indexed full-partition scan fixed by an object index + semi-naïve firing
> ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)) — see
> SPEC-12 §F3. Note also the SPEC-12 lesson recorded below: a kernel-level
> microbench win does **not** imply a workload-level win — kernel selection is
> gated on the real SPB-256 A/B, not on microbenches.

## Current results

Honest accounting. Updated when a bench moves.

### Measured

| Bench | Crate | Spec target | Measured | Verdict |
|---|---|---|---|---|
| 4-cycle, ~10⁶-edge synthetic (`benches/four_cycle.rs`) | `horndb-wcoj` | WCOJ ≥10× binary-hash | WCOJ **~0.5 s** vs binary-hash **~14–19 s** → **~30–34×** (macOS dev workstation, 2026-05-31, reconfirmed 2026-06-17). The gate is a *graph-shape* problem: the canonical skewed win case (`SyntheticGraph::skewed_four_cycle`, high-out-degree hubs + a thin closure) forces a binary join to materialise a ~3.2·10⁷-row 3-path relation while WCOJ backtracks in O(1). Correctness pinned by `tests/skewed_four_cycle.rs`. | **GREEN — Stage-1 acceptance #2 met** ([#1](https://github.com/sunstoneinstitute/horndb/issues/1)) |
| Differential fuzzer, random BGPs (`tests/differential_fuzz.rs`) | `horndb-wcoj` | zero mismatches vs binary-hash | green at 256 cases on default seed; `#[ignore]` removed | **GREEN — Stage-1 acceptance #3 met** |
| `per_tuple` — WCOJ per-tuple overhead (`benches/per_tuple.rs`) | `horndb-wcoj` | ≤**2.5 ns/tuple** (SPEC-12 NF1) | **~67 ns/tuple** (hornbench, 2026-06-30, 16-core, idle; ~13.44 ms / 200K tuples) — ~27× over target. The k==2 SIMD intersect left it **unchanged** (criterion A/B "no change"): the bench arms SIMD only at depth-0; cost is dominated by the depth-1 leapfrog over tiny 4–8-element runs (below the 64-element SIMD threshold) plus Arrow batch materialization. | **RED — NF1 not met.** Closing it needs depth-1 / materialization work, not the intersect ([#132](https://github.com/sunstoneinstitute/horndb/issues/132)) |
| `spec05_incremental_append` — single-edge append, 2,000-node chain | `horndb-closure` | incremental ≪ full recompute | incremental insert **~0.4–0.8 ms** vs full recompute **~0.5–0.7 s** → **~880–1,150×** (macOS dev workstation, 2026-06) | **GREEN** — insertion-only F6; differential-proven equal to full GraphBLAS closure (`tests/incremental.rs`) |
| owlrl materialize A/B, closure-backend swap (`horndb-bench materialize --backend rulefiring\|graphblas`) | `horndb-owlrl` + `horndb-closure` | identical closure; attribute LUBM materialize cost ([#61](https://github.com/sunstoneinstitute/horndb/issues/61)) | Both backends produce **identical** closures. LUBM-shaped synthetic (shallow taxonomy + 40 k typed instances): backend swap within noise — closure is **~0.3%** of reason time; the cost is compiled `cax-sco` type-expansion + delta apply. Closure-heavy synthetic (600-node transitive chain): closure phase **~318–517×** faster on GraphBLAS (~50–80 s → ~0.16 s). | **Parity GREEN; LUBM 3× timing gate still open and NOT closure-bound** — tracked in [#133](https://github.com/sunstoneinstitute/horndb/issues/133). Real LUBM pending (needs Jena `riot`) |
| owlrl object index A/B (`horndb-bench materialize --backend graphblas`; `MemStore` `probe(None,p,Some(o))` O(N)→O(\|extent\|), SPEC-15 fix #1) | `horndb-owlrl` | `compiled_rules_ms` drops materially; closure identical; record RSS delta ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)) | hornbench (Ryzen 7 7700, `numactl --cpunodebind=0 --membind=0`, 2026-07-07), taxonomy d=12 / 40 k inst (480,372 inferred, 520,384 total, 4 rounds), baseline `4ccaf28` vs index `d5a742e`, median of 3: **`compiled_rules_ms` ~296 → ~246 ms (−17%)**, `reason_ms` ~607 → ~555 ms (−8.5%), `apply_ms` unchanged (~268 ms), max RSS **532 → 547 MiB (+15 MiB / +2.8%)**; **inferred count identical (480,372)** — closure bit-identical (differential gates green). Modest because `compiled_rules_ms` sums **all** compiled rules and cax-sco's object-probe is one contributor; the ~4× cross-round re-derivation is fix #2 (semi-naïve, [#134](https://github.com/sunstoneinstitute/horndb/issues/134)). | **GREEN — object index landed; `compiled_rules_ms` −17%, correctness preserved, memory +2.8%** ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)) |
| `intersect` — sorted-set intersection SIMD-vs-scalar (`crates/simd/benches/intersect.rs`) | `horndb-simd` | ≥**4×** AVX-512 / ≥**2×** NEON (SPEC-12 NF2) | Microbench (2026-06-30, 50%-overlap L2-resident): Intel SPR AVX-512 **~2.5×**, AVX2 **~1.7×**; Zen4 AVX2 ≈ parity, AVX-512 **~2.5× regression** (double-pumped 512, microcoded `vpcompressq`). But the LDBC SPB-256 A/B (2026-07-01) showed SIMD **net-harmful vs scalar on the real workload on both hosts**, so the known-CPU table pins **scalar** on both. NEON not yet measured. | **AMBER — genuine microbench win, net-harmful on the real workload; scalar selected on both measured hosts** ([#132](https://github.com/sunstoneinstitute/horndb/issues/132)) |
| `lower_bound` — sorted lower-bound SIMD-vs-scalar (`crates/simd/benches/lower_bound.rs`) | `horndb-simd` | beat scalar `partition_point` | SIMD **loses 2.3×→11×** on both Zen4 and Intel SPR (2026-06-30), widening with input size — galloping + linear SIMD scan vs scalar binary search is an *algorithmic* loss. This kernel was the dominant SPB-256 SIMD-regression culprit (seek-heavy leapfrog path). | **GREEN — scalar selected everywhere** (known-CPU table + representative calibration both reject SIMD) |
| `gather` / `filter_indices_eq` — `rdf:type` scan primitives (`crates/simd/benches/{gather,filter_indices}.rs`) | `horndb-simd` | beat scalar | 2026-06-30: `gather` **~1.2–2.2× win on both hosts** (`vpgatherqq`); `filter_indices_eq` **~1.9× win sparse** (~1% match), ≈ parity dense. On the real SPB-256 workload SIMD is net-harmful, so the known-CPU table pins scalar on both measured hosts; unlisted CPUs keep `gather → AVX2` (genuine, SPB-neutral win) and get `filter_indices_eq → scalar` via representative calibration. | **GREEN — table selects scalar on measured hosts; calibration handles unlisted CPUs** |
| `dict_decode` — bulk inline-int decode (`crates/storage/benches/dict_decode.rs`) | `horndb-storage` | ≥**4×** scalar (SPEC-12 NF4) | hornbench (Ryzen 7 7700, 2026-07-07, node-0-pinned, 64Ki ids): scalar **14.74 µs** vs AVX2 **14.54 µs** → **~1.01×** (both ~4.4 Gelem/s). Inline-int decode is load/store-bound at this size, so AVX2 buys nothing — the ≥4× NF4 floor is a *compute*-speedup target the kernel can't reach on a memory-bound loop. | **RED — NF4 not met (~1.01×, not ≥4×).** SIMD is not the lever here; consistent with the broader "SIMD net-harmful/neutral on real work" finding ([#132](https://github.com/sunstoneinstitute/horndb/issues/132)) |
| `partition_scan` — `rdf:type` partition scan bandwidth (`crates/storage/benches/partition_scan.rs`) | `horndb-storage` | ≥**80% STREAM Triad** (SPEC-12 / SPEC-02 NF2) | hornbench (Ryzen 7 7700, dual-channel DDR5, 2026-07-07, `numactl --cpunodebind=0 --membind=0`, 80 MB object column): scan **34.5 GB/s** (32.12 GiB/s, 2.32 ms/iter). STREAM-Triad baseline on the same host/pin: **33.1 GB/s** full-socket (8 threads), 30.2 GB/s single-thread → scan reaches **~104% of device Triad**. A read-only scan legitimately exceeds read+write Triad; on this box a single Zen4 core already nears the dual-channel ceiling (1-thread Triad 30.2 vs 8-thread 33.1 GB/s). | **GREEN — NF2 met (~104% of STREAM-Triad ≥ 80%).** Jointly satisfies SPEC-02 acceptance #4 |
| `valued_readiness` — valued-reasoning readiness ([#11](https://github.com/sunstoneinstitute/horndb/issues/11)) | `horndb-closure` | instrument valued `(max,×)` closure to decide when custom-semiring/JIT work pays off | hornbench, 2026-06-18, weighted n-chain: valued `(max,×)` costs **~5.5×** boolean at N=500 growing to **~69×** at N=2,500 (the penalty is the scalar carrier itself — boolean's iso/bitmap closure parallelises, FP64 accumulation doesn't). Generic-kernel (UDF) penalty vs built-in FactoryKernel: **~1.0×**. | **GREEN — decision recorded:** built-in semirings suffice for a scalar carrier; PreJIT buys ≈0; custom semiring only for a structured carrier (Fork B, deferred) |
| `crosswalk` — Fork-A best-confidence crosswalk closure ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)) | `horndb-closure` | one built-in `(max,×)` closure replaces a SPARQL property-path crawl | hornbench, 2026-06-18, GTIO/SKOS-shaped layered DAG: valued closure **2.55 ms** (256 concepts) / **50.9 ms** (1,024 concepts) — **~2.3–2.6×** over boolean reachability; the end-to-end `CrosswalkGraph::best_confidence_closure` entry point (incl. extraction + ID remap) adds ≈0. | **GREEN — Fork A delivered.** Correctness pinned by `tests/crosswalk.rs`; Fork B / PreJIT deferred |
| LDBC SPB-256 `aggregation-qps` (nightly A/B vs GraphDB Free) | `horndb-sparql` | SPEC-07 NF1 — ≤2× GraphDB Enterprise (tracking [#128](https://github.com/sunstoneinstitute/horndb/issues/128)) | **HornDB 36.16 qps** (Zen4 hornbench, 2026-07-01, all-scalar SIMD table) vs **GraphDB Free ~153 qps** → **~4.2× gap**; Intel SPR hel01: 34.4 (don't compare qps across hosts — measurement windows differ). Progression: ~13 (pre-[#128](https://github.com/sunstoneinstitute/horndb/issues/128)) → ~23 (Slice 1, id-based slot rows) → ~30.8 (Slice 2; the step was bisected to the native-slot `LeftJoin`/`OPTIONAL` hash probe — the SPB mix is `OPTIONAL`-heavy) → 36.16 (SIMD known-CPU table replacing the net-harmful calibrated kernels). Streaming runtime + COUNT pushdown (#143/#144) were net-neutral on this mix. | **Tracking [#128](https://github.com/sunstoneinstitute/horndb/issues/128)** — remaining levers: probe-side join streaming, filter-aware/multi-aggregate pushdown, HTTP result streaming |

### trainmarks (DataTreehouse) — SPEC-07 SPARQL frontend, end-to-end

The [DataTreehouse **trainmarks**](https://github.com/DataTreehouse/trainmarks)
benchmark — a synthetic e-commerce graph (customers / orders / products) at
three scales with six SPARQL queries and Turtle/N-Triples I/O timing, **no OWL
reasoning** — runs end-to-end against the storage/WCOJ `HornBackend`. Unlike
the RDFox comparison, trainmarks is a public, permissively-licensed benchmark
with **no DeWitt clause**, so these numbers may be recorded and published.

Run it with `scripts/bench/trainmarks.sh` (vendored generator + queries under
`scripts/bench/trainmarks/`; native driver `crates/bench-trainmarks`). Numbers
below: **`hornbench`, release, 2026-07-06** (commit `c4645f0`, post hash
`LeftJoin`), best-of-3 warm per upstream protocol.

| operation | medium (~100K) | large (~1M) | xlarge (~10M) |
|---|---|---|---|
| read_turtle | 0.183s | 2.068s | 23.12s |
| write_turtle | 0.030s | 0.363s | 3.94s |
| write_ntriples | 0.027s | 0.341s | 3.76s |
| read_ntriples | 0.139s | 1.746s | 19.69s |
| q1 `COUNT(*)` | 0.006s | 0.069s | 1.24s |
| q2 group/sum/limit | 0.016s | 0.245s | 4.99s |
| q3 3-join + filter + limit | 0.008s | 0.133s | 2.39s |
| q4 `OPTIONAL` + `COUNT DISTINCT` | 0.021s | 0.334s | 6.80s |
| q5 `CONSTRUCT` | 0.002s | 0.038s | 1.16s |
| q6 conditional `DELETE`/`INSERT` | 0.024s | 0.682s | 11.52s |

**Status — GREEN: all six queries complete at every scale, no timeouts.**

The q4 `OPTIONAL` cliff from the first baseline (2026-06-20: 1.45s@100K →
~231s cold@1M → `TIMEOUT`@10M under the 600s upstream cap, when `LeftJoin` was
a nested loop) is gone: the slot hash-probe `LeftJoin`
([#116](https://github.com/sunstoneinstitute/horndb/issues/116),
[#128](https://github.com/sunstoneinstitute/horndb/issues/128) Slice 2) brings
q4 to **0.334s@1M (~690×) and 6.80s@10M**. The #128 aggregation rework also
moved q1 (7.92s → 1.24s @10M warm; the warm/cold split is a `COUNT`-pushdown
effect — cold q1@10M is ~4.0s). The driver's per-query watchdog (records
`TIMEOUT`, continues to the next query, matching upstream's rdflib behaviour)
is retained but no longer triggers.

Two smaller follow-ups remain open: (a) `SUM` over `xsd:double` yields
`xsd:decimal` (value correct, datatype deviates from SPARQL type promotion);
(b) no `LIMIT` pushdown. See the `HornBackend` scale notes
(`crates/sparql/tests/horn_load_hammer.rs`) for the companion ~10M load-path
memory findings (transient load-copy + 6-ordering snapshot + `stored_keys`
duplication).

### Scaffolded but not yet evaluated against targets

These benches compile and run on synthetic fixtures so future regressions are
visible. They do not yet exercise the workload the SPEC measures, and their
numbers should not be compared to the target column above.

| Bench | Crate | Notes |
|---|---|---|
| `benches/insert_throughput.rs` | `horndb-incremental` | SPEC-06 NF1/NF2 scaffold. Synthetic 10K-triple fixture — LUBM-1000 and LUBM-8000 are Stage-2 work. |
| `benches/load_lubm.rs` | `horndb-storage` | SPEC-02 F8 / acceptance #1 scaffold. |
| `benches/transitive.rs` | `horndb-closure` | SPEC-05 NF1 / acceptance #1 scaffold. |
| `benches/sameas.rs` | `horndb-closure` | SPEC-05 `owl:sameAs` equivalence-class scaffold. |
| `benches/four_cycle.rs` (binary-hash leg) | `horndb-wcoj` | Reference half of the 4-cycle comparison above. |

### Not yet running

- **LUBM-8000 materialization** (SPEC-04 acceptance #2, SPEC-02 acceptance
  #2/#3). Gated on the storage + rule engine being usable on real corpora.
- **ORE 2015 OWL 2 RL fragment full pass.** Ten-ontology subset is wired up
  (`../harness/ore2015-selected.toml`); the full corpus expansion is Stage-2
  work (`../TASKS.md` MEDIUM).

### Running — LDBC SPB nightly (published)

`.github/workflows/nightly.yml` brings HornDB up per run (serving the
prepared flat closure, no reasoning), drives the SPB aggregation query mix
against `/query` + `/update`, and records the **full driver report** into the
trend DB (per-query counts/latencies, editorial breakdown, totals — queryable
via `harness report --suite ldbc-spb-256 --metric <name>`). The A/B reference
is **GraphDB Free 10.8.14** (licence-free; 11.x requires a licence), brought
up per run so neither engine competes for RAM during the other's measurement;
no licence restriction on publishing GraphDB Free numbers.

Current scale is *feasible scale* — the 512 k-triple materialized SPB closure,
aggregation-only (`editorialAgents=0`, headline metric `aggregation-qps`).
Scaling to true SF=0.256 (256 M triples) + editorial agents is tracked in
`../TASKS.md`. Current numbers: the `aggregation-qps` row in *Measured* above.

### Running, internal only (no published numbers)

**A/B vs RDFox** (SPEC-01 F10) — implemented and runnable via
`../scripts/bench/compare-rdfox.sh` (see `../scripts/bench/README.md`). Times
HornDB against RDFox on identical inputs for bulk import, transitive closure,
and OWL 2 RL materialization. Per the DeWitt-clause note under *Baselines*,
results are written to gitignored `scripts/bench/results/` and are never
committed. Outstanding: a real-LUBM materialization workload and wiring the
comparison into CI / the trend DB.

## Reproducing the numbers

All measured numbers above come from `cargo bench` invocations against the
relevant crate, **run on `hornbench`** (see *Reference hardware*). Use
`--quick` for development sweeps; record both means **and** the criterion HTML
reports (under `target/criterion/`) for any number quoted in `TASKS.md`, a
commit message, or a published artefact.

```bash
# WCOJ acceptance #2 — the headline Stage-1 perf bench
cargo bench -p horndb-wcoj --bench four_cycle

# WCOJ / SPEC-12 NF1 — per-tuple overhead microbench
cargo bench -p horndb-wcoj --bench per_tuple

# WCOJ correctness — differential fuzzer
cargo test -p horndb-wcoj --test differential_fuzz

# SPEC-12 SIMD kernels
cargo bench -p horndb-simd --bench intersect
cargo bench -p horndb-simd --bench lower_bound
cargo bench -p horndb-simd --bench gather
cargo bench -p horndb-simd --bench filter_indices

# SPEC-12 storage consumers
cargo bench -p horndb-storage --bench dict_decode
cargo bench -p horndb-storage --bench partition_scan

# SPEC-06 incremental insert throughput
cargo bench -p horndb-incremental --bench insert_throughput

# SPEC-02 storage — LUBM load throughput
cargo bench -p horndb-storage --bench load_lubm

# SPEC-05 closure — transitive, sameAs, incremental, valued, crosswalk
cargo bench -p horndb-closure --bench transitive
cargo bench -p horndb-closure --bench sameas
cargo bench -p horndb-closure --bench incremental
cargo bench -p horndb-closure --bench valued_readiness
cargo bench -p horndb-closure --bench crosswalk
```

End-to-end conformance and benchmark runs go through the harness binary; see
[`../README.md`](../README.md#run-the-conformance-harness) and
[`../crates/harness/README.md`](../crates/harness/README.md). Results persist
to `target/harness.sqlite` and are queryable via `harness report`.

## Updating this document

When a bench moves into *Measured* (or moves between RED and GREEN), update
the relevant row, link the issue or plan that closed the gap, and update the
corresponding entry in `../TASKS.md` and the Status field in
`architecture.md` in the same commit. Keep rows to *current state + pointer*:
the measurement history lives in the harness trend DB (the harness records
`(commit-sha, suite, hardware, throughput-metric, latency-metric)` per run —
SPEC-01), and the investigation narratives live in `plans/`. This file is the
human-readable index into that store, not a replacement for it.
