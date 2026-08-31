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
| **Oxigraph** (Rust, RocksDB) | Closest architectural peer: Rust SPARQL 1.1 store, no reasoner — serves the same flat closure as HornDB. MIT/Apache-2.0, so numbers publish freely (no DeWitt clause). | Nightly LDBC SPB-256 A/B reference, two legs: as-loaded (`oxigraph`) and `oxigraph optimize`d store (`oxigraph-optimized`); both built by `../crates/harness/scripts/bootstrap-oxigraph-spb.sh`, pinned via `OXIGRAPH_VERSION` in `nightly.yml`. |
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
| Whole-graph scan (`scan_graph`) | cost tracks the **graph**, never the store | SPEC-28 S2 acceptance #4 — the Graph Store Protocol `GET` / whole-graph `PUT` diff hot path |
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
| Closure retraction, small delta over growing store (SPEC-24 S2, [#211](https://github.com/sunstoneinstitute/horndb/issues/211)) — support-counting vs recompute | output-sensitive: cost ∝ closure delta + frontier, **not** store size | `benches/closure_retraction.rs` — measured: pending hornbench |

### SPEC-06 — DBSP incremental maintenance (`horndb-incremental`)

| Metric | Target | Baseline |
|---|---|---|
| Steady-state insert/retract latency (LUBM-1000 warm) | ≤**100 ms** | NF1 / acceptance #1 (jointly owned with SPEC-04 NF3) |
| Sustained insert throughput (warm LUBM-8000) | ≥**100K triples/sec** | NF2 / acceptance #2 |
| Query-latency degradation under sustained write load | ≤**2×** no-write baseline | acceptance #2 |
| Pending delta size between checkpoints | ≤**5%** of main store | NF3 |
| Small-delta retraction: delta-incremental vs recompute fallback (SPEC-24 S1, [#210](https://github.com/sunstoneinstitute/horndb/issues/210)) | ≥**10×** at N=256 | `benches/retraction_throughput.rs` — see *Measured* below |

> **Stage 1 reality check:** NF1 and NF2 are *Stage-2 gates*. Stage-1 ships
> only the criterion benchmark scaffold (`benches/insert_throughput.rs`) on a
> synthetic 10K-triple fixture so regressions become visible as the real
> engine lands. Rule-path retraction is now delta-incremental (SPEC-24 S1,
> [#210](https://github.com/sunstoneinstitute/horndb/issues/210)); the
> remaining Stage-2 gaps are in
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
| WCOJ per-tuple overhead (`per_tuple`) | ≤**5 ns/tuple** | DuckDB ~2 ns; SPEC-03 NF1 is the source of truth (the ≤2.5 ns SIMD-epic #132 figure is superseded) |
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
| `per_tuple` — WCOJ per-tuple overhead (`benches/per_tuple.rs`) | `horndb-wcoj` | ≤**5 ns/tuple** (SPEC-03 NF1, source of truth; the ≤2.5 ns SIMD-epic #132 figure is superseded) | Two cases, same-session A/B on hornbench (Ryzen 7 7700, `numactl --cpunodebind=0 --membind=0`, 2026-07-27), baseline `c6e682b` vs columnar `184f791`. **Marginal hot path** `wide_4x100k` (high fan-out, the NF1 measurement): **8.51 → 2.74 ns/tuple** (1.702 ms → 547.3 µs / 200K) — **3.1×**, and ~5× below the pre-#237 ~14.4 ns. **Descent-bound** `two_star_50k` (50k subjects × 4 rows each — a full trie descent per 4 tuples, so it measures amortized descent, not the marginal cost, and structurally cannot reach NF1): **56.1 → 49.0 ns/tuple** (11.23 ms → 9.79 ms / 200K), −13%. #237 landed the galloping descent (`run_end`/`seek_gallop`, replacing the wide-range bisect) + bulk leaf materialization (`push_run_chunk`, replacing per-value `push_row`), reaching 8.4 ns; a `perf` profile then showed the residual was the row→column input copy (~46% of the marginal profile) plus its dedup (~33%). #239 removed both by storing `VecTripleSource` column-major, so a trie level's values are read in place and the leaf `active_run` is a slice, not a copy. No regression elsewhere: same-session `four_cycle/wcoj` **195.8 → 173.9 ms** (−11%), `binary_hash` unchanged (5.76 → 5.82 s). | **GREEN — NF1 met: marginal 2.74 ns/tuple ≤ 5 ns** ([#239](https://github.com/sunstoneinstitute/horndb/issues/239), [#237](https://github.com/sunstoneinstitute/horndb/issues/237)) |
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
| LDBC SPB-256 `aggregation-qps` (nightly A/B vs GraphDB Free) | `horndb-sparql` | SPEC-07 NF1 — ≤2× GraphDB Enterprise (gap-closing work now tracked under [#204](https://github.com/sunstoneinstitute/horndb/issues/204)) | **HornDB 50.25 qps** (Zen4 hornbench, nightly 2026-08-24, commit `8a5ed81`) vs **GraphDB Free 151.96 qps** → **3.02× gap**; the same night's Oxigraph legs: 38.08 as-loaded / 38.05 optimized, so HornDB leads its closest architectural peer by **~1.32×**. Don't compare qps across hosts (Intel SPR hel01 measured 34.4 on the older code; measurement windows differ). Progression: ~13 (pre-[#128](https://github.com/sunstoneinstitute/horndb/issues/128)) → ~23 (Slice 1, id-based slot rows) → ~30.8 (Slice 2, native-slot `LeftJoin`/`OPTIONAL` hash probe — the SPB mix is `OPTIONAL`-heavy) → ~36 (SIMD known-CPU table replacing the net-harmful calibrated kernels) → ~43 on 2026-07-20 (WCOJ galloping descent + bulk leaf materialization [#237](https://github.com/sunstoneinstitute/horndb/issues/237) and SPEC-23 Phase 2 heuristic rewrites [#202](https://github.com/sunstoneinstitute/horndb/issues/202) landed together — not bisected) → ~45.7 on 07-27 (columnar SoA `VecTripleSource`, [#257](https://github.com/sunstoneinstitute/horndb/issues/257)) → **~50 since 08-14** (SPEC-28 named-graph phases 1–4; the 07-31→08-13 nightly gap means this step is not bisected). Streaming runtime + COUNT pushdown (#143/#144) were net-neutral on this mix. | **Ahead of Oxigraph, 3.02× behind GraphDB Free** — gap down from ~4.2× (2026-07-01) but still outside the ≤2× NF1 target. The levers once listed here as "remaining" (probe-side join streaming, filter-aware/multi-aggregate pushdown via SPEC-21, HTTP result streaming via SPEC-22) have all landed and did not close it. Next lever: cost-based join planning (SPEC-23 Phase 4, [#204](https://github.com/sunstoneinstitute/horndb/issues/204)) |
| `graph_scan` — graph-scoped access paths (`crates/storage/benches/graph_scan.rs`) | `horndb-storage` | `scan_graph` cost tracks the graph, not the store (SPEC-28 S2 acceptance #4); warm footprint ≤**50 B/triple** (SPEC-02 NF1) | hornbench (16-core Debian 6.12, rustc 1.90.0, 2026-07-30, commit `abadb4b`): scanning the **same** 10-triple graph costs **1.113 µs** in a 1,000-graph / 1M-quad store and **1.145 µs** in a 2,000-graph / 2M-quad store — **+2.9% for a doubled store**, i.e. flat in store size. `graph_len` on that graph: **13.35 ns** (it sums a cached per-partition live count, so it is O(predicates in graph) with no row scan). Partition overhead at 1,000 triples/graph (5 predicates, so ~200 rows/partition): **32.08 B/quad**, identical across both corpora. | **GREEN — O(graph)-not-O(store) confirmed; 32.08 B/quad within the ≤50 B/triple NF1 budget.** Note the corpus is 1,000 triples *per graph*: this measures scan cost against graph **count**, and does **not** yet answer SPEC-28's "thousands of **small** graphs vs per-partition overhead" risk, where each graph holds a handful of triples and the ~16 B/partition constant dominates. That shape needs its own corpus ([#265](https://github.com/sunstoneinstitute/horndb/issues/265)) |
| Bulk N-Triples import (`Store::load_ntriples_file`, `examples/load_curve.rs`) | `horndb-storage` | ≥**1 M triples/sec** (SPEC-02 F8) | hornbench (Ryzen 7 7700, Debian 6.12, rustc 1.90.0, 2026-08-31, commit `548e850`), trainmarks xlarge (9,995,000 triples), **one thread**, load plus a first read, median of 3: **9.71s = 1.03 M triples/s** at the shipped 65,536-triple batch — parse, interning and index build included. The same corpus took **48.21s (0.21 M/s)** at `e6b6836`, immediately before HDB-84 replaced the per-batch partition rebuild with an appended run; peak RSS fell 2,205 → 1,547 MiB with it. Cost is now flat in the batch size (full curve below). | **GREEN on this corpus — F8 met (1.03 M/s ≥ 1 M/s).** Scope: in-memory tier, one parse thread, a 10M-triple synthetic e-commerce graph. The LUBM-100 / LUBM-8000 acceptance gates (#1, #2) are separate and still unmeasured |
| `retraction_throughput` — small-delta retraction A/B, delta-incremental vs Stage-1 recompute fallback (SPEC-24 S1, [#210](https://github.com/sunstoneinstitute/horndb/issues/210)) | `horndb-incremental` | incremental ≥**10×** recompute at N=256 (#210 acceptance) | hornbench (Ryzen 7 7700, Linux 6.12, rustc 1.90.0, 2026-07-20), warm SC-chain fixture (N `SC` edges + N `TYPE` facts, ~N² derived rows), steady-state retract/tick/re-assert/tick cycle at the interior N−4 cut: incremental **11.8 ms / 110 ms / 1.15 s** vs recompute fallback **57.8 ms / 1.21 s / 28.97 s** at N=64/128/256 → **4.9× / 11.0× / 25.1×**. Same host, `insert_throughput` (insertion-path no-regression companion, first hornbench baseline for the scaffold): insert/10 **14.1 µs**, insert/50 **2.67 ms**, insert/100 **30.3 ms**. Known crossover: a *bulk* cut (delta ≈ half the store) runs at ~0.8× recompute — the expected DBSP trade-off; the gate is small-delta by design. LUBM-scale rerun deferred until SPEC-24 S4 engine wiring gives the circuit real consumers. | **GREEN — #210 acceptance met (25.1× ≥ 10× at N=256)** |

### trainmarks (DataTreehouse) — SPEC-07 SPARQL frontend, end-to-end

The [DataTreehouse **trainmarks**](https://github.com/DataTreehouse/trainmarks)
benchmark — a synthetic e-commerce graph (customers / orders / products) at
three scales with six SPARQL queries and Turtle/N-Triples I/O timing, **no OWL
reasoning** — runs end-to-end against the storage/WCOJ `HornBackend`. Unlike
the RDFox comparison, trainmarks is a public, permissively-licensed benchmark
with **no DeWitt clause**, so these numbers may be recorded and published.

Run it with `scripts/bench/trainmarks.sh` (vendored generator + queries under
`scripts/bench/trainmarks/`; native driver `crates/bench-trainmarks`). Numbers
below: **`hornbench`, release, 2026-08-24** (commit `b020f53`, post delta-merge
snapshot reuse), best-of-3 warm per upstream protocol.

| operation | medium (~100K) | large (~1M) | xlarge (~10M) |
|---|---|---|---|
| read_turtle | 0.180s | 2.076s | 22.92s |
| write_turtle | 0.035s | 0.370s | 3.73s |
| write_ntriples | 0.026s | 0.338s | 3.54s |
| read_ntriples | 0.139s | 1.730s | 20.21s |
| q1 `COUNT(*)` | 0.004s | 0.044s | 0.705s |
| q2 group/sum/limit | 0.014s | 0.214s | 3.73s |
| q3 3-join + filter + limit | 0.006s | 0.076s | 1.23s |
| q4 `OPTIONAL` + `COUNT DISTINCT` | 0.016s | 0.249s | 4.72s |
| q5 `CONSTRUCT` | 0.002s | 0.030s | 0.587s |
| q6 conditional `DELETE`/`INSERT` | 0.003s | 0.036s | 0.452s |

**Status — GREEN: all six queries complete at every scale, no timeouts.**

The q4 `OPTIONAL` cliff from the first baseline (2026-06-20: 1.45s@100K →
~231s cold@1M → `TIMEOUT`@10M under the 600s upstream cap, when `LeftJoin` was
a nested loop) is gone: the slot hash-probe `LeftJoin`
([#116](https://github.com/sunstoneinstitute/horndb/issues/116),
[#128](https://github.com/sunstoneinstitute/horndb/issues/128) Slice 2) brings
q4 to **0.251s@1M and 4.69s@10M**. The #128 aggregation rework also moved q1
(7.92s → 0.709s @10M warm; the warm/cold split is a `COUNT`-pushdown effect —
cold q1@10M is ~3.7s). The driver's per-query watchdog (records `TIMEOUT`,
continues to the next query, matching upstream's rdflib behaviour) is retained
but no longer triggers.

q6 (`DELETE`/`INSERT … WHERE`) dropped 11.52s → **0.452s @10M (25×)** with
HDB-82: a small quad delta is now merged into the cached WCOJ snapshot instead
of forcing a full re-index of all six orderings. Its cold run paid the first
snapshot build until HDB-97 made that build lazy (below).

#### `q1`'s cold-start tax was a second, redundant snapshot build (HDB-97, 2026-08-26)

`q6` always runs first in the driver, and its `WHERE` clause resolves the
`DefaultStrict` scope (SPARQL Update reads the store's default graph, absent
`USING`/`WITH`); `q1`-`q5` are plain `SELECT`/`CONSTRUCT` and resolve
`DefaultUnion`. Both are whole-store WCOJ snapshots, each memoised
independently — so on trainmarks' single-default-graph data (where the two
scopes read the exact same triples), `q6`'s cold run built one full
six-sort-pass snapshot and `q1`'s cold run — the first `SELECT` to run —
built a second one from scratch for what was, in substance, identical data.
That second build *was* `q1`'s entire cold-start cost.

`wcoj_snapshot` now clones an already-cached twin scope's sorted data
(`O(n)`) instead of rebuilding (`O(n log n)`) once the second scope is
actually asked for — see `crates/sparql/INTEGRATION-NOTES.md`'s "GRAPH
patterns" section. Controlled A/B on `hornbench` (commit `e2b290e` with and
without the fix, back-to-back, xlarge/10M):

| operation | before | after | change |
|---|---|---|---|
| `q1_count_cold` | 3.684s | 0.655s | **−82%** |
| `q1_count` (warm) | 0.409s | 0.407s | ~0 |
| `q6_delete_insert_cold` | 4.533s | 4.390s | ~0 |
| `q6_delete_insert` (warm) | 1.244s | 1.300s | ~0 |
| q2/q3/q4/q5, cold and warm | — | — | ~0 (±2%, noise) |

No other query moved outside noise in either direction — the fix is scoped
to exactly the redundant-build cost it targets. (The warm-path `q6`/`q1`
absolute values here are higher than the `b020f53` table above; that drift
predates this change — see the commits between the two on `main` — and is
out of this fix's scope.)

#### `q6`'s cold start built five trie orderings nobody read (HDB-97, 2026-08-28)

`VecTripleSource` materialised all six trie orderings when a snapshot was
built. Instrumenting a whole trainmarks run showed only **three** are ever
requested (`Spo`, `Pso`, `Pos`) and `q6`'s cold run requests **one** — so five
of the six sort passes, and ~1.2 GB of index at 10M triples, were built for
nobody.

Orderings are now built on first use. One — the *anchor*, `Pso` — is built
with the snapshot; the rest derive from it when something asks. `Pso` is the
anchor because `horndb-storage`'s snapshot scan already yields
predicate-major, subject-major order, so building it is a linear pass rather
than a sort. Cost shifts from "six sorts before the first query" to "one sort
per ordering a query actually reads, when it reads it". The warm path gets the
same treatment for free: `apply_delta` merges a delta into the orderings that
exist, not all six.

Controlled A/B on `hornbench` (`419f921` with and without the change,
back-to-back, xlarge/10M; the "after" column is the mean of two runs that
agreed within 1% except `q6_cold`, which spanned 1.71–2.00s):

| operation | before | after | change |
|---|---|---|---|
| `q6_delete_insert_cold` | 5.039s | 1.859s | **−63%** |
| `q6_delete_insert` (warm) | 1.314s | 0.521s | **−60%** |
| `q1_count_cold` | 0.629s | 0.465s | −26% |
| `q3_join_3_entities_cold` | 1.163s | 1.773s | **+52%** |
| q2/q4/q5 cold, all six warm | — | — | ~0 (±2%, noise) |
| the four I/O rows | — | — | ~0 (±2%, noise) |

`q3` is the honest other side of the trade: it is the only query that reads
`Pos`, and it now pays that one sort itself instead of finding it prebuilt.
Summed over the six cold runs the suite is **2.6s faster**, and no warm number
regressed. A cheaper derive for `q3` — `Pos` from `Pso` is a per-predicate
re-sort of an already predicate-major index, not a global sort — is the
follow-up, tracked as `HDB-98`.

Memory falls with the sort passes: a 10M-triple snapshot holds one ordering
(~240 MB) plus whatever queries ask for, against six (~1.4 GB) before.

#### `Pos` derives from the `Pso` anchor by block sort, not a global sort (HDB-98, 2026-08-28)

HDB-97 (above) left `q3` paying a full global sort of all n rows to derive
`Pos`. It does not need one. The anchor is `Pso` and `Pos` is `(predicate,
object, subject)` — both predicate-major, so the two orderings group rows into
the *same* contiguous predicate blocks and differ only *within* a block. No row
crosses a block boundary.

`TripleColumns::derive_blockwise` therefore sorts each predicate block on its
own two remaining columns: O(n log(n/b)) for b blocks against O(n log n), with a
per-block working set small enough to stay in cache. The anchor is already
deduplicated, so a block's rows are distinct as a pair and no dedup pass is
needed. Only `Pos` qualifies from a `Pso` anchor; `Spo`, `Sop`, `Osp` and `Ops`
do not share its level-0 axis and still take the global sort.

Isolated derive, 10M rows over 12 predicates (laptop, release): **339ms →
224ms (−34%)**.

End-to-end A/B on `hornbench` (`19d035b` vs `889193c`, trainmarks xlarge/10M,
the two sides interleaved over two rounds, best of two each):

| operation | before | after | change |
|---|---|---|---|
| `q3_join_3_entities_cold` | 1.665s | 1.500s | **−9.9%** |
| `q3_join_3_entities` (warm) | 1.181s | 1.137s | −3.8% |
| every other query, cold and warm | — | — | ~0 (±2%, noise) |
| the four I/O rows | — | — | ~0 (±2%, noise) |

`q3` is the suite's only `Pos` reader, so it is the only row that can move —
which is what the table shows. The derive cost isolates cleanly as cold minus
warm: **484ms → 363ms (−25%)**, tracking the isolated figure.

`q3` cold does **not** return to its pre-HDB-97 1.163s, and cannot: back then
all six orderings were built eagerly at snapshot time, so `q3`'s cold run found
`Pos` prebuilt and `q6` paid for it. HDB-97 moved that build into whichever
query reads `Pos` first; HDB-98 makes the build itself a quarter cheaper. The
work is real and someone has to pay it — the suite total is the number that
matters, and it improved under both changes.

#### Parallel chunked parsing does not move the read columns (HDB-83, 2026-08-24)

`oxttl` can split a document into N independently parseable chunks, so the
bulk loaders gained a slice-based parallel entry point beside the streaming
one (`crates/storage/src/loader/parallel.rs`). It does **not** pay, and the
default is serial (`HORNDB_LOAD_THREADS=auto` turns it on). Measured on
`hornbench` (16 cores, release, commit `49ddaf3`, trainmarks xlarge ≈10M
triples):

| operation | 1 thread | 16 threads | change |
|---|---|---|---|
| `read_turtle` (driver, end-to-end) | 23.60s | 22.72s | −3.7% |
| `read_ntriples` (driver, end-to-end) | 20.08s | 18.97s | −5.5% |

Best of two runs each; the other columns of the trainmarks table are
unaffected. A few percent, against a parser that on its own runs 9× faster on
16 threads.

The phase breakdown says why. Same host, same data, N-Triples:

| phase | 1 thread | 4 | 16 |
|---|---|---|---|
| parse only (chunk-local, nothing crosses threads) | 5.41s | 1.65s | 0.67s |
| parse + intern (both on the chunk threads) | 8.08s | 2.96s | 2.06s |
| full `Store` load (parse + intern + tier insert) | 40.1s | 43.4s | 46.6s |

Turtle behaves the same, shifted up: parse only 8.34s → 0.91s at 16 threads.

Three findings, in order of usefulness:

1. **The index build, not the parse, owns the load.** Parsing is ~13% of a
   `Store` load and interning ~7%; the remaining ~80% is
   `Tier::insert_quad_batch` building six trie orderings per predicate. Even a
   free parser would leave a 10M-triple load above 32s. *Two corrections since:
   the tier writes two physical layouts per predicate, not six (HDB-88), and
   the ~80% was the per-batch partition rebuild, which HDB-84 removed — see
   the batch-size curve below. It was also specific to the batched `Store`
   loader; on the one-shot path HDB-85 measured the tier at 12-14%.*
2. **Interning is not the serialisation point.** The dictionary's reverse-map
   `RwLock<Vec<Term>>` was the suspected bottleneck; it is not. Parse+intern
   scales 3.9× from 1 to 16 threads. The follow-up idea of thread-local
   dictionaries with a parallel merge is therefore not worth filing.
3. **More parse threads make the full load slower** (40.1s → 46.6s), because
   the serial tier insert has to free terms allocated on 16 other threads
   while those threads keep allocating.

The shipped design keeps interning on the calling thread in document order, so
a parallel load produces the same term ids as a serial one and the two are
byte-for-byte interchangeable; it measured 48.2s vs 49.4s (1 vs 16 threads) on
the same corpus. Re-measure with `HORNDB_LOAD_THREADS=auto` once the ordering
build is cheaper — that is where the load-path work belongs. HDB-84 has since
made it cheaper (below), so that re-measurement is now worth doing.

Two smaller follow-ups remain open: (a) `SUM` over `xsd:double` yields
`xsd:decimal` (value correct, datatype deviates from SPARQL type promotion);
(b) no `LIMIT` pushdown. See the `HornBackend` scale notes
(`crates/sparql/tests/horn_load_hammer.rs`) for the companion ~10M load-path
memory findings (transient load-copy + 6-ordering snapshot + `stored_keys`
duplication).

#### Where bulk-load time actually goes (HDB-85, 2026-08-24)

Measured with the `storage_load_phase_*` counters (SPEC-17 §5.4.1) on
`hornbench`, trainmarks xlarge (9,995,000 triples), serial load, commit
`25f4110`. Phases are listed in the order a load runs them.

| phase | read_turtle | % | read_ntriples | % |
|---|---|---|---|---|
| `parse` | 10.176s | 43.5 | 6.455s | 31.7 |
| `dedupe` | 6.150s | 26.3 | 5.855s | 28.8 |
| `intern` | 1.909s | 8.2 | 1.823s | 9.0 |
| `live_keys` | 1.448s | 6.2 | 1.397s | 6.9 |
| `group` | 1.414s | 6.0 | 1.386s | 6.8 |
| `build` | 1.292s | 5.5 | 1.287s | 6.3 |
| `stage` | 0.308s | 1.3 | 0.303s | 1.5 |
| `merge` | 0.140s | 0.6 | 0.137s | 0.7 |
| `copy_forward` | ~0 | 0 | ~0 | 0 |
| **accounted** | **22.837s** | **97.6** | **18.643s** | **91.7** |
| **measured total** | 23.389s | | 20.342s | |

Phase names as of the measured commit. HDB-84 has since changed two of them on
the write path this table covers: `copy_forward` is no longer emitted at all
(nothing is carried forward), and `build` now covers only the batch's own rows,
with the rest of the work moving to a new `merge_runs` phase on the first read.
The numbers below are correct for `25f4110` and are not re-stated —
`docs/metrics.md` has the current definitions.

Three things this overturns:

- **Parsing is the largest single phase**, not a rounding error — and it is
  tokenisation, not the batch build. Timing the `Vec<(OxTerm, …)>`
  materialisation on its own (`materialize` phase) splits `parse` as:

  | | read_turtle | | read_ntriples | |
  |---|---|---|---|---|
  | tokenisation | 9.627s | 94.5% | 5.885s | 90.0% |
  | `materialize` | 0.565s | 5.5% | 0.657s | 10.0% |
  | **`parse`** | **10.192s** | | **6.542s** | |

  An earlier revision of this section blamed the materialisation for the whole
  phase, by comparing HDB-83's *N-Triples* "parse only 5.41s" against the
  *Turtle* `parse` phase. Wrong: HDB-83 measured Turtle separately at 8.34s,
  which agrees with the 9.627s tokenisation figure above. The allocation costs
  0.57s, not ~4.8s.
- **The tier is 12–14% of a load, not ~80%.** `group` + `copy_forward` +
  `merge` + `build` total 2.85s of 23.39s (Turtle) and 2.81s of 20.34s
  (N-Triples). The index build is not the bottleneck, and `copy_forward` is
  free on a load into an empty store. (`copy_forward` being free here is what
  the one-shot path looks like; on the batched path it was the whole cost, which
  is HDB-84 below.)
- **Every term is interned twice.** `dedupe`
  (`HornBackend::insert_oxrdf_batch_in_graph`) interns all three terms to build
  a `QuadKey`, then `intern` (`Store::apply_quads`) interns them all again for
  storage's own ids. This section originally read the two phases together as
  "8.06s (34%) of interning"; HDB-90 split `dedupe` with counters and the real
  figure is **4.77s (20%)** — the rest of `dedupe` is `intra_batch.insert` and
  the term moves. `dedupe` is the dearer of the two passes because it takes
  every dictionary *miss*, not because of its `live_keys` lookup, which is free
  here. See the sub-phase table below.

`live_keys` adds a further 1.45s building a 10M-entry `HashSet<QuadKey>` that
exists for `INSERT DATA` idempotency and cannot hit on a load into an empty
store.

Ranked targets, all of them above the tier: tokenisation (9.6s), the duplicate
intern (1.9s recoverable, see below), the term moves inside `dedupe` (1.5s),
`intra_batch.insert` (1.4s), and the `live_keys` build (1.4s).

#### Inside `dedupe`: interning is half of it, `live_keys.contains` is free (HDB-90, 2026-08-24)

`dedupe`'s 26% was never split by a counter — HDB-57 R2 inferred it by assuming
the three `d.intern()` calls in the loop cost what the separately-instrumented
`intern` phase costs (1.909s). They do not. Measured with the opt-in
`dedupe_*` sub-counters (`HORNDB_DEDUPE_SUBPHASES=1`, see `docs/metrics.md`) on
`hornbench`, trainmarks xlarge, serial, commit `66e3302`; each column is the
mean of two runs that agreed within 2%. These runs predate the snmalloc swap
(#293): the split is a share of `dedupe`, so it holds, but the absolute seconds
are glibc-allocator numbers and the intern column — which allocates on every
dictionary miss — is the one most likely to move when they are re-measured.

| sub-phase | read_turtle | % of `dedupe` | read_ntriples | % of `dedupe` |
|---|---|---|---|---|
| the three `d.intern()` calls | 2.893s | 46.7 | 2.707s | 45.9 |
| `entries.push` + the term moves | 1.540s | 24.9 | 1.520s | 25.8 |
| `intra_batch.insert` | 1.421s | 22.9 | 1.390s | 23.6 |
| `live_keys.contains` | 0.042s | 0.7 | 0.041s | 0.7 |
| **accounted** | **5.896s** | **95.2** | **5.658s** | **95.9** |
| `dedupe`, uninstrumented | 6.195s | | 5.899s | |

Three findings:

- **Interning is 47% of `dedupe`, not the ~31% the 1.909s assumption implies.**
  The two passes are not symmetric: `dedupe` runs first, so every term is a
  dictionary *miss* (hash, allocate, insert, push to the reverse `Vec`), while
  `Store::apply_quads` re-interns the same terms as pure *hits*. A miss costs
  ~1.55x a hit here. Strings → ids therefore costs 2.893s + 1.879s = **4.77s
  (20.1% of the 23.74s Turtle load)**, against HDB-57 R2's inferred 3.8s / 16%.
- **`live_keys.contains` costs nothing on a bulk load** — 0.7% of `dedupe`, 42ms
  for 10M probes. A bulk load is one `insert_oxrdf_batch` call, and `live_keys`
  is only populated in phase 2, *after* the loop. So the set is empty for every
  probe and the lookup returns on the capacity check. The cost R2 budgeted to it
  does not exist on this path; it appears only on `INSERT DATA` into a populated
  store.
- **A quarter of `dedupe` is `entries.push` and the term moves** — 1.540s, a
  sub-phase R2 did not account for at all. It hashes nothing. `entries` is a
  10M-element `Vec<(QuadKey, 3 × oxrdf::Term)>` — about 1 GB — that phase 2 then
  re-copies into `to_store`.

So quad-level dedup does **not** cost 1.9x string interning. Counting hash-set
work on both sides: strings 4.77s against quads 2.88s (`intra_batch` 1.421s +
`live_keys` 1.422s + `contains` 0.042s). Interning leads by 1.7x, the opposite
of R2's direction. Folding `group`'s 1.418s into the quad side still leaves
strings ahead (4.77s vs 4.30s).

What this does *not* change: **HDB-87 is still worth ~1.9s.** Removing the
duplicate intern leaves whichever pass survives doing the misses, so the saving
is the hit pass — the 1.879s `intern` row, 7.9% of the Turtle load. R2 reached
the same figure from the wrong premise (that both passes cost ~1.9s).

Method and its limits: splitting a per-triple loop needs a clock read between
each step, which inflates `dedupe` by 13-16%. The `dedupe_clock` counter times
an empty interval per iteration (16.5 ns on this host), and each sub-phase above
has one subtracted; the raw counters are exported unadjusted. The corrected
figures land 4-5% under the uninstrumented `dedupe`, and that residue is the
loop-iterator advance plus the part of each `Instant::now()` that falls outside
the measured intervals — it is instrumentation, not a missing sub-phase. The
same procedure on a synthetic 1.2M-triple corpus reproduced the uninstrumented
total to within 2%.

#### Parse does not parallelise, and the phase counters say where it goes

Same host and data, `HORNDB_LOAD_THREADS=16` against the serial run:

| phase (read_turtle) | 1 thread | 16 threads | change |
|---|---|---|---|
| `parse` | 10.192s | 9.874s | −3.1% |
| `materialize` | 0.565s | 0.770s | **+36%** |
| `dedupe` | 6.058s | 6.098s | +0.7% |
| `intern` | 1.851s | 1.843s | −0.4% |

HDB-83 measured *chunk-local* parsing scaling 8.34s → 0.91s on 16 threads. The
whole of that win is consumed inside `for_each_*_batch`. HDB-83's own wording
names the reason — its figure was "chunk-local, **nothing crosses threads**",
while the real loader ships every parsed triple across a channel to a sink on
the calling thread. `materialize` getting *worse* with more threads is the
tell: the sink is touching memory allocated on other cores.

The mechanism is `oxttl`'s API. `SliceTurtleParser::Item = Result<Triple, _>`
and `oxrdf::Triple` owns its `String`s, so the parser allocates a copy of every
term even though the source slice outlives the parse. At xlarge that is 30M
allocations made on the parse threads and freed on the main thread — the case
glibc `malloc` handles worst, since a cross-thread free takes the owning
arena's lock. The workspace sets no `#[global_allocator]`.

Term reuse is the lever: a 2M-triple sample of `xlarge.nt` holds 6M term
occurrences but only **577,706 distinct terms** (10.4:1). Interning from `&str`
borrowed out of the source slice would allocate only on a dictionary miss,
cutting allocations roughly 10× — and every survivor is a real dictionary
entry rather than transient churn. Tracked in HDB-86.

**The allocation story above is real but it is not why `parse` fails to
scale.** The next two sections measure both: swapping the allocator moves
`parse` ~10%, while the channel lookahead bound moves it 5×.

#### The parse channel starves its own producers (HDB-86, 2026-08-24)

`parse_chunks_ordered` drains the per-chunk receivers strictly in document
order (`for rx in receivers`), and each chunk's channel holds
`CHANNEL_DEPTH * BATCH` = 2 × 8,192 = **16,384 triples**. At xlarge with 16
threads a chunk is 624,687 triples, so a parse thread can run **2.6% of its
chunk** ahead before it blocks on `send`.

The consequence is that the parse is serial by construction: while the consumer
works through chunk 0, threads 1–15 each fill 16,384 triples and park. Exactly
one producer makes progress at a time. The predicted gain from 16 threads is
just the one-time head start of 15 × 16,384 = 245,760 triples, **2.5% of the
document** — against the 4.8% measured above.

`HORNDB_LOAD_CHANNEL_DEPTH` (diagnostic knob) confirms it directly. Same host
and data, 16 threads, `--reserve-triples 10000000`, one run per cell:

| depth | lookahead/thread | `parse` (ttl) | vs depth 2 | `parse` (nt) | read_turtle | read_ntriples |
|---|---|---|---|---|---|---|
| **2** (default) | 16,384 | 10.068s | — | 5.432s | 23.245s | 19.113s |
| 8 | 65,536 | 9.274s | 1.09× | 5.037s | 22.415s | 18.642s |
| 32 | 262,144 | 6.294s | 1.60× | 3.514s | 19.656s | 17.178s |
| 128 | 1,048,576 | **1.960s** | **5.14×** | **1.345s** | **15.038s** | **14.857s** |

snmalloc shows the same curve (8.605s → 1.716s, 5.02×), so the effect is the
channel bound, not the allocator.

At depth 128 the Turtle tokenise half is **1.319s**, against the 0.91s HDB-83
measured for chunk-local parsing with nothing crossing threads. The parallel
parse win was never lost to the thread boundary — it was lost to a buffer two
batches deep.

Two properties make this cheap:

- **Term ids stay deterministic.** Only buffering changes; the drain order is
  untouched, so triples, dictionary contents, and term ids are unchanged.
  `tests/parallel_loader.rs` compares interned ids directly
  (`assert_same_store`) and passes at depths 1, 2, 7 and 64.
- **It is a constant, not a design.** None of HDB-86's gating decision about
  document-order id assignment has to be settled to take this win.

The cost is memory, and it is modest — the buffered batches are bounded by the
document, and the load's peak is dominated by the store and the batch `Vec`
rather than the channel. Peak RSS, same host and data, 16 threads, default
(grow-on-demand) batch:

| depth | peak RSS | vs depth 2 | read_turtle |
|---|---|---|---|
| 2 | 9,596 MiB | — | 23.274s |
| 8 | 9,595 MiB | −0.0% | 22.435s |
| 32 | 9,591 MiB | −0.1% | 19.613s |
| 64 | 10,482 MiB | +9.2% | 15.858s |
| 128 | 10,875 MiB | **+13.3%** | **15.041s** |

**+13% peak memory for a 5.1× `parse` and −35% end-to-end.** Depth 32 is free
in memory terms and still worth 1.6×.

Both open questions — what the default should be, and whether the bound
belongs in triples rather than batches — are settled in HDB-94 below.

#### The buffer is now a triple budget, and `parse` scales (HDB-94, 2026-08-24)

The per-chunk `CHANNEL_DEPTH` is replaced by a **total** in-flight budget in
triples, shared out across the chunks
(`load_buffer_triples()`, `HORNDB_LOAD_BUFFER_TRIPLES`, default
**8,388,608**). Two things follow from making it a total:

- Peak buffer memory no longer grows with `HORNDB_LOAD_THREADS`. More threads
  split the same budget more ways. Under the old per-chunk constant both the
  cost and the benefit scaled with the thread count, which is why neither was
  legible in the constant.
- The number states what it buys. `8 << 20` triples against a 9,995,000-triple
  document says "buffer most of it"; `CHANNEL_DEPTH = 2` said nothing without
  first working out the chunk size.

`hornbench`, trainmarks xlarge (9,995,000 triples), commit `76d8b0a`,
`--load-only --reserve-triples 10000000`, one run per cell. Peak RSS is
`VmHWM` sampled at 5 Hz; the preallocated 10M-triple batch is in every cell,
so compare the deltas, not the absolute figure (the HDB-86 table above was
taken without the preallocation and starts ~4 GiB lower).

Budget sweep, 16 threads:

| budget | per-chunk lookahead | `parse` (ttl) | vs 262 k | `parse` (nt) | read_turtle | peak RSS |
|---|---|---|---|---|---|---|
| 262,144 (old bound at 16 threads) | 16,384 | 8.677s | — | 5.640s | 21.368s | 13,719 MiB |
| 1,048,576 | 65,536 | 8.014s | 1.08× | 5.364s | 20.793s | 13,824 MiB |
| 2,097,152 | 131,072 | 7.175s | 1.21× | 4.700s | 19.890s | 13,797 MiB |
| 4,194,304 | 262,144 | 5.506s | 1.58× | 3.632s | 18.187s | 13,675 MiB |
| **8,388,608 (default)** | 524,288 | **2.381s** | **3.64×** | **1.481s** | **15.002s** | 14,424 MiB (**+5.1%**) |
| 16,777,216 | 1,048,576 | 1.743s | 4.98× | 0.930s | 14.513s | 14,893 MiB (+8.6%) |

Thread sweep at the default budget — this is the property that was missing:

| threads | `parse` (ttl) | vs 1 thread | `parse` (nt) | read_turtle | read_ntriples |
|---|---|---|---|---|---|
| 1 (no channel — one chunk runs inline) | 9.126s | — | 6.348s | 21.920s | 18.623s |
| 2 | 5.470s | 1.67× | 3.842s | 17.959s | 15.947s |
| 4 | 3.738s | 2.44× | 2.302s | 16.208s | 14.679s |
| 8 | 2.676s | 3.41× | 1.670s | 15.260s | 13.966s |
| 16 | 2.381s | 3.83× | 1.481s | 15.002s | 13.748s |

Second runs of three cells agree to within 5% on `parse` and 0.1% end-to-end
(262 k: 8.690s / 21.386s; 8M: 2.498s / 15.019s; 1 thread: 9.182s / 21.971s).

**Why 8M and not 16M.** 8M is the knee. The step from 4M to 8M is worth 2.3× on
`parse`; the step from 8M to 16M is worth 1.37× on `parse` but only 3% more
end-to-end, and costs another 3.5 points of peak memory. Worst case the budget
holds ~1 GiB of parsed terms (~134 B/triple measured), and only for a document
big enough to fill it.

SPEC-02 NF1 (≤50 B/triple) bounds the **warm tier**, so it does not bound this
buffer — the buffered terms are transient parse output, freed as the drain
consumes them, and none of them survive into the store. The relevant guard is
that the budget is absolute: it caps at ~1 GiB whatever the corpus and whatever
the thread count, where the old constant had no corpus-independent cap at all.

At the shipped `HORNDB_LOAD_THREADS=1` the buffer costs exactly nothing: one
chunk skips the channel and parses inline.

**Still open: the parse-thread default.** The thread sweep above is the bench
driver's path (parse → `Vec` → `HornBackend::insert_oxrdf_batch`), where 16
threads is now a 32% end-to-end win rather than HDB-83's 16% loss. Flipping
`HORNDB_LOAD_THREADS` to `auto` needs the same sweep against a real `Store`
load, whose tier insert is what regressed in HDB-83. Tracked separately.

#### Swapping the allocator moves `parse` ~10% (HDB-86 E1, 2026-08-24)

E1 asked how much of `parse` is allocator policy. `hornbench`, trainmarks
xlarge (9,995,000 triples), commit `00b9203`, median of 3 runs per cell,
`--load-only`. Host confirmed quiet for every leg; the reps are interleaved by
allocator and time-separated, and per-cell spread is ≤3.6% on 11 of 12 cells.

Measured on the **shipped** grow-on-demand path, `parse` looks inert — but that
is two effects cancelling:

| read_turtle | `parse` 1 thr | `parse` 16 thr | `materialize` 1 thr |
|---|---|---|---|
| system (glibc) | 10.449s | 9.918s | 0.592s |
| mimalloc | 10.798s | 9.692s | 1.641s |
| snmalloc | 10.268s | 9.658s | 1.771s |

`materialize` triples because the parse batch reaches ~1 GB and growing it is a
few reallocs of one very large block: glibc serves those from `mmap` and grows
them with `mremap` (page-table edits only), while mimalloc and snmalloc copy.
That is a genuine cost of swapping the allocator, but it belongs to the `Vec`,
not the parse, and it masks the tokenise win.

With the batch preallocated (`--reserve-triples 10000000`, one run per cell)
`materialize` returns to ~0.6s for all three and the real effect shows:

| read_turtle, reserved | `parse` 1 thr | `parse` 16 thr | end-to-end 1 thr |
|---|---|---|---|
| system (glibc) | 10.269s | 9.945s | 23.418s |
| mimalloc | 9.407s (−8.4%) | 8.725s (−12.3%) | 22.070s (−5.8%) |
| snmalloc | 9.185s (−10.6%) | 8.622s (−13.3%) | 21.948s (−6.3%) |

read_ntriples behaves the same: `parse` 6.032s → 5.583s (snmalloc, −7.4%),
end-to-end 20.090s → 18.081s (−10.0%).

Conclusions:

1. **The allocator is worth ~10% of `parse` and ~6–10% end-to-end**, snmalloc
   ahead of mimalloc, and it does not require any design commitment.
2. **It does not fix scaling.** 1→16 threads stays within −3% to −13% whichever
   allocator is used. The channel depth above is what fixes that.
3. **The realloc regression was the bench driver's, not the engine's.** Only
   the driver accumulates a whole document into one `Vec`; the engine's loaders
   batch at `BATCH = 8192` and never build a block that large.

**Outcome: snmalloc adopted.** All four shipped binaries (`bench-trainmarks`,
`harness`, `serve`, `bench-rdfox`) set the `#[global_allocator]`, each behind a
default-on `snmalloc` feature that is the revert switch. mimalloc lost the A/B
and is not carried. The driver now preallocates its parse batch by estimating
the triple count from the mean line length of a 1 MiB prefix, which is what
makes the swap a clean win rather than a wash.

`serve` is what the LDBC SPB-256 nightly measures, and E1 only measured the
bulk-load path — the nightly is the gate on the query path, and disabling the
feature is the revert.

Verified on the shipped build against its own revert switch (same binary source,
`--no-default-features` for the system-allocator leg, so both preallocate and
only the allocator differs; one run per cell, quiet host):

| xlarge, end-to-end | shipped (snmalloc) | revert (system) | change |
|---|---|---|---|
| read_turtle, 1 thread | 21.999s | 23.543s | **−6.6%** |
| read_turtle, 16 threads | 21.630s | 23.221s | −6.9% |
| read_ntriples, 1 thread | 17.733s | 19.969s | **−11.2%** |
| read_ntriples, 16 threads | 17.980s | 19.290s | −6.8% |

Against the pre-E1 baseline (system allocator, grow-on-demand batch, median of
3) the combined change is read_turtle 23.658s → 21.999s (−7.0%) and
read_ntriples 20.693s → 17.733s (−14.3%). Preallocation alone accounts for
little of that on glibc (23.658s → 23.543s), which is expected — `mremap` was
already cheap; it earns its place by removing the penalty snmalloc would
otherwise pay.

#### Repeated small batches into the tier were quadratic in the call count (HDB-84, 2026-08-31)

`Tier::insert_quad_batch` rebuilt every predicate partition a batch touched:
copy each existing row into a builder, append the batch's rows, sort the lot,
then materialise fresh Arrow columns and Roaring side-sets — and, above the
1M-row hot threshold, sort again for the object-major layout. N batches into
one predicate paid O(existing) N times, so the tier's cost tracked **how the
caller chunked its input**, not how many triples it inserted.

A partition is now a list of sorted **runs** — blocks of rows whose
concatenation is the partition. A batch appends one run: only that batch is
sorted, and the rows already stored are shared by `Arc`, neither copied nor
re-sorted. The merged columns every read path needs are built once, on the
first read, by the same sort-and-dedup the single-shot build always used. A
batched load pays one sort at the end instead of one per batch.

`hornbench`, trainmarks xlarge N-Triples (9,995,000 triples) loaded through
`Store::load_ntriples_file`, serial parse, system allocator, commits `e6b6836`
(before) and `548e850` (after). The same two binaries, the two sides
interleaved, median of 3. The reported time is the load **plus a first read**,
which is what forces the merge — a load nobody reads leaves work undone, so
the loader call on its own is not a comparable number. Peak RSS is `VmHWM`
sampled at 5 Hz on a separate single run per cell. Driver:
`cargo run --release -p horndb-storage --example load_curve -- <file> <batch>`.

| triples per insert call | calls | before | after | change | peak RSS before → after |
|---|---|---|---|---|---|
| 8,192 | 1,221 | 315.31s | 9.95s | **31.7×** | 2,638 → 1,600 MiB (−39%) |
| 65,536 (the loader default) | 153 | 48.21s | 9.71s | **4.97×** | 2,205 → 1,547 MiB (−30%) |
| one call for the whole document | 1 | 9.74s | 9.45s | 1.03× | 1,566 → 1,534 MiB (−2%) |

Run-to-run spread is under 1% on every cell except 65,536-after (9.70–10.13s,
4.4%); the three before/after pairs never overlap.

The review follow-ups were re-confirmed against `548e850` on the same host,
two interleaved reps per cell. `e274ebb` (the `merge_runs` phase and the
`MAX_RUNS` cap) landed within 1% on every cell, peak RSS within 1 MiB.
`2aeac6e` then stopped releasing the runs before the merged columns are built —
closing an unwind window at the cost of one transient copy of the rows — and
measured **1–2% slower in all four pairs**: 9.94s vs 9.82s at 65,536 and
10.12s vs 9.92s at 8,192. Small, inside the spread the table already reports,
and consistent enough in direction to be real rather than noise. Peak RSS did
not move (1,537–1,594 MiB against 1,543–1,597 MiB), which says the load's peak
sits somewhere other than the merge, not that the merge peak is unchanged.

The cap is not reached by any of this: 8,192-triple batches produce 1,221 runs
against a cap of 4,096.

Read the "after" column downwards, not across: it is flat. Load cost no longer
depends on how the input was chunked, and the batched path now costs what the
one-call path costs. Before, the same 10M triples spanned 9.7s to 315s on
chunking alone. At the shipped 65,536 that is **48.2s → 9.7s**, which is
**1.03 M triples/s** end to end on one thread — parse, intern and index build
included. It is also the figure HDB-83 reported as "full `Store` load 40.1s"
against a parse of 5.41s: the gap was the rebuild.

Peak memory drops for the same reason. The old rebuild held the outgoing
partition and its replacement at once, so a batched load peaked at roughly two
copies of the largest partition on top of the store; appending a run does not.

The batch size is now settable — `HORNDB_LOAD_BATCH_TRIPLES`, default 65,536 —
so the curve is reproducible on any commit carrying the knob.
`crates/storage/tests/parallel_loader.rs::batch_size_does_not_change_the_store`
pins that it is a throughput knob only: five batch sizes, streaming and
parallel, each producing an identical store down to the interned term ids.

**This does not move trainmarks `read_turtle` / `read_ntriples`, and cannot.**
Those legs do not use the batched loader. The driver parses into one `Vec` and
calls `HornBackend::insert_oxrdf_batch`, which reaches the tier **once** with
all 10M rows — the one-call row above, where there is nothing to amortise.
HDB-85 measured that path's whole tier share at 12–14%. Measured anyway, same
host, same commits, `--load-only --reserve-triples 10000000`, median of 3
interleaved runs:

| operation | before | after | change |
|---|---|---|---|
| `read_turtle` | 21.618s | 21.586s | −0.1% (noise) |
| `read_ntriples` | 18.083s | 18.161s | +0.4% (noise) |

The path this fixes is `Store::load_ntriples_file` / `load_turtle_file`, which
the harness and the SPARQL server use to load a document into a `Store`.

It also unblocks the parse-thread default. HDB-94 left "flipping
`HORNDB_LOAD_THREADS` to `auto` needs the same sweep against a real `Store`
load, whose tier insert is what regressed in HDB-83" open. That tier insert is
no longer what regressed, so the sweep is now worth running.

#### Where HornDB sits against the other eleven engines

Upstream publishes its own numbers in the report page
(<https://datatreehouse.github.io/trainmarks/> — the values are embedded in
`index.html` as a `DATA` constant; the repo tree carries no `results/` dir).
Eleven engines, August 2026, **Apple M3 Max / 36 GB**, best-of-3 warm — the same
protocol, generator seed, and query files we run, on different hardware from our
**hornbench** (Ryzen 7 7700, 16 threads, 124 GB). Treat the comparison as
directional, not as a controlled A/B: only a same-host run settles a close call.

Seconds at xlarge (~10M triples); HornDB from the run above, the rest from
upstream:

| operation | HornDB | oxigraph | maplib | qlever | jena | rdf4j | graphdb | rdflib |
|---|---|---|---|---|---|---|---|---|
| read_turtle | 22.92 | 15.25 | 9.53 | 8.75 | 16.72 | 24.90 | 57.10 | 191.6 |
| read_ntriples | 20.21 | 13.13 | 9.36 | 8.65 | 16.49 | 13.88 | 54.21 | 154.6 |
| write_turtle | 3.73 | 3.58 | 1.34 | — | 6.74 | 19.34 | — | 134.7 |
| write_ntriples | 3.54 | 3.46 | 0.72 | — | 3.89 | 13.18 | — | 17.51 |
| q1 `COUNT(*)` | 0.705 | 0.786 | 0.073 | 0.002 | 1.837 | 0.678 | 1.414 | 37.0 |
| q2 group/sum/limit | 3.73 | 2.11 | 0.019 | 0.002 | 3.28 | 0.900 | 2.86 | 32.7 |
| q3 3-join + filter | 1.23 | 0.390 | 0.040 | 0.005 | 0.329 | 0.165 | 0.616 | 7.60 |
| q4 `OPTIONAL` + `COUNT DISTINCT` | 4.72 | 1.72 | 0.031 | 0.003 | 3.09 | 1.16 | 3.66 | 48.1 |
| q5 `CONSTRUCT` | 0.587 | 0.754 | 0.081 | 0.592 | 0.377 | 0.269 | 4.38 | 9.00 |
| q6 `DELETE`/`INSERT` | 0.452 | 0.035 | 0.020 | — | 0.028 | 0.022 | 0.174 | 0.787 |

Reading it:

- **Writes are our strongest showing.** 3rd of 7 on both serialization paths at
  10M, level with Oxigraph.
- **Parsing is mid-pack** — 1.5× behind Oxigraph, 2.7× behind QLever, but 2.4×
  ahead of GraphDB and 8× ahead of rdflib.
- **Queries degrade with scale.** At medium HornDB is mid-pack and fastest of
  all eleven on q5; at 10M it is 10th of 11 on q2, q3, and q4 — the
  aggregation- and join-heavy queries. Our scaling curve is steeper than the
  field's.
- **Oxigraph beats us here**, the opposite of the LDBC SPB-256 nightly where
  HornDB leads it 50.25 vs 38.08 qps on identical hardware. Hardware alone does
  not explain the flip: SPB is `OPTIONAL`-heavy with small result sets (where
  the hash-probe `LeftJoin` pays), while trainmarks q2/q4 are full-scan
  aggregations over all 10M triples. The weakness is bulk aggregate throughput,
  not join shape — the same gap SPEC-23 Phase 4 cost-based planning targets.
- **q6 after HDB-82** is 7th of 8 rather than last by 400×: a real win, still
  13× behind Oxigraph.

Two caveats on the upstream column. QLever's 2–5 ms for q1–q4 at 10M and
Neo4j's 11 ms for q4 are result-cache hits under best-of-3 warm, not query
execution — read them as an upper bound on caching, not as a target. And the
benchmark is authored by DataTreehouse, the vendor of maplib, which wins most
rows.

### Scaffolded but not yet evaluated against targets

These benches compile and run on synthetic fixtures so future regressions are
visible. They do not yet exercise the workload the SPEC measures, and their
numbers should not be compared to the target column above.

| Bench | Crate | Notes |
|---|---|---|
| `benches/insert_throughput.rs` | `horndb-incremental` | SPEC-06 NF1/NF2 scaffold. Synthetic 10K-triple fixture — LUBM-1000 and LUBM-8000 are Stage-2 work. First hornbench numbers recorded in the SPEC-24 S1 row in *Measured* above (2026-07-20). |
| `benches/load_lubm.rs` | `horndb-storage` | SPEC-02 F8 / acceptance #1 scaffold. |
| `benches/transitive.rs` | `horndb-closure` | SPEC-05 NF1 / acceptance #1 scaffold. |
| `benches/sameas.rs` | `horndb-closure` | SPEC-05 `owl:sameAs` equivalence-class scaffold. |
| `benches/closure_retraction.rs` | `horndb-closure` | SPEC-24 S2 A/B: support-counting vs recompute deletion, small delta over a growing store ([#211](https://github.com/sunstoneinstitute/horndb/issues/211)). Pending hornbench. |
| `benches/four_cycle.rs` (binary-hash leg) | `horndb-wcoj` | Reference half of the 4-cycle comparison above. |
| `benches/insert_retract.rs` (`insert_10k`, `retract_then_scan_10k`) | `horndb-storage` | SPEC-25 S1 per-tuple MVCC scaffold — insert-only baseline + retract-then-read cost. Builds and runs locally; measure on hornbench (deferred, [#242](https://github.com/sunstoneinstitute/horndb/issues/242)), which also carries the NF4 write-amp comparison (stamp-columns-on-CoW vs delete-bitmap sidecars, CoW vs in-place append). |

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
via `harness report --suite ldbc-spb-256 --metric <name>`). The A/B references
are **GraphDB Free 10.8.14** (licence-free; 11.x requires a licence; no licence
restriction on publishing its numbers) and **Oxigraph 0.5.9**, the latter run as
two legs — the store as bulk-loaded (label `oxigraph`) and an `oxigraph
optimize`d copy (label `oxigraph-optimized`) — so both configurations keep their
own trend series. Each engine is brought up per run so none competes for RAM
during another's measurement. The trend DB keeps a 90-day rolling window
(`harness prune --keep-days 90` in the nightly).

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

# SPEC-02 F8 — bulk-load wall time vs tier batch size (HDB-84). Not criterion:
# one load per invocation, so sweep the batch size yourself. 0 = one insert call.
cargo run --release -p horndb-storage --example load_curve -- data/xlarge.nt 65536 3

# SPEC-28 S2 storage — graph-scoped access paths (also prints B/quad to stderr)
cargo bench -p horndb-storage --bench graph_scan

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
