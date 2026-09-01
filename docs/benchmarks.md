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
| Bulk N-Triples import (`Store::load_ntriples_file`, `examples/load_curve.rs`) | `horndb-storage` | ≥**1 M triples/sec** (SPEC-02 F8) | hornbench (Ryzen 7 7700, Debian 6.12, rustc 1.90.0), trainmarks xlarge (9,995,000 triples), load plus a first read, at the shipped 65,536-triple batch — parse, interning and index build included. **One thread: 9.57s = 1.04 M triples/s** (2026-09-01, commit `ca4933e`, median of 5). **At the shipped parse-thread default** (`auto` → 8 on this host, HDB-96): **6.09s = 1.64 M triples/s** (commit `c9c1b34`, this branch pre-rebase, median of 3) — measured before HDB-95, so it does not carry that change; HDB-95 moved this path's *memory* rather than its time (the one-thread figure went 9.49s → 9.57s across it, inside the spread). The same corpus took **48.21s (0.21 M/s)** at `e6b6836`, immediately before HDB-84 replaced the per-batch partition rebuild with an appended run; peak RSS fell 2,205 → 1,547 MiB with it, and to **1,338 MiB** when HDB-95 dropped the datatype IRI out of the dictionary key. Cost is flat in the batch size (full curve below). | **GREEN on this corpus — F8 met at both settings (1.64 M/s threaded, 1.04 M/s on one thread).** Scope: in-memory tier, a 10M-triple synthetic e-commerce graph, **empty store**. A 10% append into that loaded store runs at 0.57 M/s on the same path, and at **0.69 M/s** through `HornBackend` since HDB-102 gave `apply_quad_batch` the append-run path (0.04 M/s when HDB-91 first measured it below). The LUBM-100 / LUBM-8000 acceptance gates (#1, #2) are separate and still unmeasured |
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

**The two read legs are one-parse-thread numbers.** The driver takes its thread
count from `loader::load_threads()`, whose default HDB-96 changed from 1 to
`auto`. `scripts/bench/trainmarks.sh` pins `HORNDB_LOAD_THREADS=1` so this table
keeps one basis and stays comparable across hosts; unpinning it rebases
`read_turtle` / `read_ntriples` and nothing else here. Re-measure the table in
the same commit if you change it.

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
one (`crates/storage/src/loader/parallel.rs`). It did **not** pay at the time,
and the default was serial. HDB-94 and HDB-84 later removed both reasons and
HDB-96 flipped the default to `auto` — the conclusion below is superseded;
keep it for the history of *why*. Measured on
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
the same corpus. That re-measurement is done: HDB-84 made the ordering build
cheaper and HDB-96 re-ran the sweep against a real `Store` load, where 8
threads is a 2.3× win rather than a loss.

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

Phase names as of the measured commit. **Corrected by HDB-91:** an earlier
revision of this paragraph said HDB-84 had stopped this path emitting
`copy_forward`. It has not. This table is the `HornBackend` path, which reaches
the tier through `Store::insert_quads` -> `Store::apply_quads` ->
`apply_quad_batch`, and HDB-84 rewrote `insert_quad_batch` only. `copy_forward`
is ~0 here because the whole 10M arrives in one call into an empty store, with
nothing to carry; on an append it is 30% (see HDB-91 below). `merge_runs`
belongs to the bulk-loader path, not to this one. The numbers are correct for
`25f4110`; `docs/metrics.md` has the current definitions. **The `group` and
`build` rows have since been roughly halved** — see "Cutting the
`apply_quad_batch` hash tables" (HDB-88) below for the current figures.

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
  here. See the sub-phase table below. **Fixed in HDB-87** (two sections down):
  `intern` is now zero on this path and the `stage` phase no longer exists.

`live_keys` adds a further 1.45s building a 10M-entry `HashSet<QuadKey>` that
exists for `INSERT DATA` idempotency and cannot hit on a load into an empty
store. **Removed by HDB-89** (below): storage decides membership,
so the phase no longer exists.

Ranked targets, all of them above the tier: tokenisation (9.6s), the duplicate
intern (1.9s — recovered by HDB-87), the term moves inside `dedupe` (1.5s, half
recovered with it), `intra_batch.insert` (1.4s), and the `live_keys` build
(1.4s).

#### Inside `dedupe`: interning is half of it, `live_keys.contains` is free (HDB-90, 2026-08-24)

**Phase names as of the measured commit.** HDB-89 deleted `live_keys` and with
it the `dedupe_contains` sub-phase; the `QuadKey` build that row also covered is
now inside `dedupe_intra`.

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
the same figure from the wrong premise (that both passes cost ~1.9s). Measured
outcome: 2.5-2.8s, because carrying ids instead of terms also shrinks `dedupe`
and deletes `stage` — next section.

Method and its limits: splitting a per-triple loop needs a clock read between
each step, which inflates `dedupe` by 13-16%. The `dedupe_clock` counter times
an empty interval per iteration (16.5 ns on this host), and each sub-phase above
has one subtracted; the raw counters are exported unadjusted. The corrected
figures land 4-5% under the uninstrumented `dedupe`, and that residue is the
loop-iterator advance plus the part of each `Instant::now()` that falls outside
the measured intervals — it is instrumentation, not a missing sub-phase. The
same procedure on a synthetic 1.2M-triple corpus reproduced the uninstrumented
total to within 2%.

#### Interning once removes the second pass and the term buffer (HDB-87, 2026-08-31)

`HornBackend::insert_oxrdf_batch_in_graph` now keeps the ids it interned in
phase 1 and hands them to `Store::insert_quad_ids`, instead of handing the
terms back for a second dictionary pass. Three costs go with it: the `intern`
phase, the `stage` phase, and part of `dedupe` — the surviving-entry buffer is
four ids per row (32 B) instead of a `QuadKey` plus three heap-backed
`oxrdf::Term`s (~1 GB at this scale).

Controlled A/B on `hornbench`, trainmarks xlarge (9,995,000 triples), release
+ snmalloc, `--load-only --reserve-triples 10000000`, serial parse (the
`HORNDB_LOAD_THREADS` default of 1 at the time; `auto` since HDB-96). Before `4d8d2b9`, after `ac4fd5b`; three
runs of each, interleaved before/after, median reported. Host confirmed quiet
(load average 0.00 at start).

| | before (ttl) | after (ttl) | before (nt) | after (nt) |
|---|---|---|---|---|
| **read_turtle / read_ntriples** | **21.630s** | **19.087s** (**-11.8%**) | **18.275s** | **15.511s** (**-15.1%**) |
| `parse` | 9.108s | 9.465s | 5.925s | 5.976s |
| ` ` of which `materialize` | 0.592s | 0.602s | 0.489s | 0.509s |
| `dedupe` | 5.876s | 5.327s | 5.603s | 5.010s |
| `intern` | 1.927s | **0** | 1.864s | **0** |
| `stage` | 0.267s | **gone** | 0.214s | **gone** |
| `live_keys` | 1.280s | 1.285s | 1.298s | 1.274s |
| `group` | 1.383s | 1.391s | 1.384s | 1.363s |
| `build` | 1.192s | 1.227s | 1.207s | 1.225s |
| `merge` | 0.087s | 0.162s | 0.117s | 0.119s |
| `copy_forward` / `invalidate` | ~0 | ~0 | ~0 | ~0 |
| **accounted** | 21.121s | 18.857s | 17.611s | 14.967s |

Run spread: read_turtle 21.630 / 21.619 / 21.890 before, 19.165 / 18.709 /
19.087 after; read_ntriples 18.275 / 18.052 / 18.299 before, 15.524 / 15.511 /
15.501 after. The two distributions do not overlap on either leg.

- **`intern` is zero**, which is the acceptance criterion: the phase is emitted
  only by the term-based `Store::apply_quads`, and the bulk path no longer
  reaches it. `stage` is not zero but removed — it covered building the key and
  `to_store` vectors, and there is nothing left to stage.
- **`dedupe` drops 0.55s (ttl) / 0.59s (nt)**, roughly a third of the 1.54s
  HDB-90 attributed to `entries.push` and the term moves. The rest stays: phase
  1 still consumes the incoming `Vec<(Term, Term, Term)>`, so the terms are
  still dropped, just one buffer earlier.
- **End-to-end beats the phase deltas** by 0.16s (ttl) and 0.09s (nt). That is
  the teardown of the ~1 GB `entries` buffer, which no counter covered.
- Turtle `parse` reads 0.36s higher after the change and N-Triples `parse` is
  flat (+0.05s). This cannot be an effect of the change: the driver closes the
  `parse` phase before it constructs the `HornBackend` and calls
  `insert_oxrdf_batch`, so no code touched here runs inside that interval. And
  `read_turtle` is whole-function wall clock, so the reading is already inside
  the -11.8% — if it were real it would mean the headline understates the win.

Predicted 1.9s plus "most of" 1.54s; delivered 2.5-2.8s. The prediction that
the term moves would largely vanish was too strong — half of them did.

#### Deleting `live_keys` removes a phase and 1.8 GiB (HDB-89, 2026-09-01)

`HornBackend` kept `live_keys`, a `HashSet<QuadKey>` mirroring every live quad,
so `INSERT DATA` idempotency and `DELETE DATA` no-op detection were O(1)
lookups. Storage already decides both: `Tier::apply_quad_batch` inserts only
what is not visible after the batch's deletions and returns the true
`inserted`/`retracted` counts. The mirror was therefore a second copy of an
answer the store already had, and on a load into an empty store every lookup
missed. It is gone; the backend returns storage's counts, and the single-triple
insert path short-circuits on the new `StoreSnapshot::contains_quad`, an
O(log rows) binary search over the predicate partition's sorted columns.

Controlled A/B on `hornbench` (16 cores), trainmarks xlarge (9,995,000
triples), release + snmalloc, `--load-only --reserve-triples 10000000`, serial
parse (`HORNDB_LOAD_THREADS=1`). Before `902cb1e`, after `dd6032c`; three runs
of each, interleaved before/after, median reported. Host confirmed quiet (load
average 0.12 at start).

| | before (ttl) | after (ttl) | before (nt) | after (nt) |
|---|---|---|---|---|
| **read_turtle / read_ntriples** | **18.686s** | **17.194s** (**-8.0%**) | **15.669s** | **14.052s** (**-10.3%**) |
| `parse` | 9.105s | 9.034s | 5.930s | 5.910s |
| ` ` of which `materialize` | 0.534s | 0.536s | 0.500s | 0.500s |
| `dedupe` | 5.255s | 5.207s | 5.042s | 5.029s |
| `live_keys` | 1.291s | **gone** | 1.302s | **gone** |
| `group` | 1.412s | 1.403s | 1.388s | 1.397s |
| `build` | 1.222s | 1.222s | 1.206s | 1.200s |
| `merge` | 0.151s | 0.143s | 0.143s | 0.188s |
| `copy_forward` / `invalidate` | ~0 | ~0 | ~0 | ~0 |
| **accounted** | 18.435s | 17.010s | 15.011s | 13.724s |
| **peak RSS** | **11,323,108 KB** | **9,423,412 KB** (**-1.81 GiB**) | | |

Run spread: read_turtle 18.686 / 18.864 / 18.585 before, 17.146 / 17.194 /
17.262 after; read_ntriples 15.669 / 15.797 / 15.510 before, 14.160 / 13.966 /
14.052 after; peak RSS 11,322,156 / 11,331,436 / 11,323,108 KB before,
9,432,572 / 9,400,520 / 9,423,412 KB after. Neither distribution overlaps on
either leg, and the RSS legs are 200x apart relative to their own spread.

- **The `live_keys` phase is not reduced, it no longer exists.** With it goes
  `dedupe_contains`, the opt-in sub-phase that timed the `live_keys.contains`
  probe (0.7% of `dedupe`, HDB-90). The `QuadKey` build that sub-phase also
  covered moved into `dedupe_intra`.
- **End-to-end beats the phase delta** by 0.20s (ttl) and 0.32s (nt): 1.49s and
  1.62s measured against a 1.29s / 1.30s phase. The remainder is tearing down
  the set, which no counter covered — the same effect HDB-87 saw when it
  deleted the ~1 GB `entries` buffer.
- **Peak RSS falls 1.81 GiB, 16.8%.** The driver keeps the Turtle backend alive
  across the N-Triples load, so the peak holds two of these sets: ~0.9 GiB
  each. That is a 10M-entry `hashbrown` table of 32-byte keys (~554 MB at the
  16.8M-slot capacity) plus the transient old table during its last doubling.
  At 10M triples the mirror alone was ~55 B/triple — on its own over SPEC-02
  NF1's ≤50 B/triple budget for the whole store.
- **No write-path regression.** The full trainmarks suite at `large` (1,001,000
  triples, one run each) moved no query outside noise, and `q6`, the only
  UPDATE, went 0.0254s -> 0.0222s. `contains_quad` replaces a hash probe with a
  binary search on the single-triple insert path, but that path already pays
  `apply_quad_batch`'s whole-partition rebuild on every triple that is actually
  new, which is orders of magnitude larger than either lookup. (HDB-102 has
  since removed that rebuild from add-only calls; the conclusion — that the
  lookup is not what costs here — is unchanged, and the margin is smaller.)

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

A one-chunk load never allocates the buffer at all, so `HORNDB_LOAD_THREADS=1`
is how to get the pre-HDB-96 footprint back.

**Settled, in HDB-96 below.** The thread sweep above is the bench driver's path
(parse → `Vec` → `HornBackend::insert_oxrdf_batch`), where 16 threads is a 32%
end-to-end win rather than HDB-83's 16% loss. The same sweep against a real
`Store` load now agrees — 2.3× on Turtle, 2.0× on N-Triples — and the default
has flipped to `auto`, capped at 8. The buffer measured here is the bulk of
what that costs in memory.

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

It also unblocked the parse-thread default. HDB-94 left "flipping
`HORNDB_LOAD_THREADS` to `auto` needs the same sweep against a real `Store`
load, whose tier insert is what regressed in HDB-83" open. HDB-96 ran it: the
tier phases are flat in the thread count, the regression does not reproduce,
and the default is now `auto` (see below).

#### Appending to a loaded store: the incremental phase table (HDB-91, 2026-09-01)

Every load number above comes from a load into an **empty** store. This section
measures the other case — a store that already holds a corpus, then takes
another one — because that is the case SPEC-25 S2 (persistent dictionary)
exists for, and it had never been profiled.

`hornbench` (Ryzen 7 7700, 16 threads, Debian 6.12, rustc 1.90.0), commit
`186f9c6`, snmalloc, one parse thread (the default at the time). Base corpus:
trainmarks
`xlarge.nt`, 9,995,000 triples. Append corpus: 1,002,000 triples (10% of the
base) in two vocabulary flavours that differ *only* in whether their terms are
already interned:

- **overlap** — every term but the new order IRI is one the base already
  contains. 167,000 new dictionary entries.
- **fresh** — identical shape and identical predicates, but the entity IRIs sit
  in a second namespace and the literal values come from disjoint ranges.
  501,426 new dictionary entries. That figure is both counted by the driver and
  reproducible from the generator: 167,000 order IRIs + 167,000 customers +
  167,000 products + 1 class + 5 statuses + 420 dates (the date grid is
  `lcm(5, 12, 28)`, not 5x12x28). The 20 new quantity literals add nothing —
  `xsd:integer` literals are value-encoded inline (`TermKind::InlineInt`) and
  never allocate a dictionary entry. For the same reason the append makes
  2,839,000 dictionary probes, not 3,006,000: the quantity column never probes.

Both are produced by `scripts/bench/trainmarks/generate_append.py`. Predicates
are the same in both on purpose: they decide which tier partitions the append
lands in, so holding them fixed leaves dictionary hit rate as the only
variable. Driver: `crates/bench-trainmarks/src/bin/incremental_load.rs`, which
loads the base, forces the deferred merge (as a reopened store would be), then
appends and reports the `storage_load_phase_*` deltas for the append alone.
Three interleaved reps per cell, median reported; run-to-run spread is under 3%
on every cell.

##### Two entry points, and only one of them got HDB-84's fix

`Tier` has two insert paths, and HDB-84 changed one of them:

| entry point | reached from | carries existing rows forward? |
|---|---|---|
| `insert_quad_batch` | `Store::insert_triples`, the bulk loaders (`load_ntriples_file` / `load_turtle_file`) | no — appends a run, merges on first read (`merge_runs`) |
| `apply_quad_batch` | `Store::apply_quads` **and `Store::insert_quads`**, hence all of `HornBackend` — every SPARQL ingest, `INSERT DATA`, and the trainmarks driver | yes, at the time of this measurement — `copy_forward` + a full partition rebuild per batch. **HDB-102 has since removed it** for every predicate a batch does not delete from; the rows below are the pre-HDB-102 state |

`Store::insert_quads` is a thin wrapper over `apply_quads` (SPEC-28 S6), so the
whole SPARQL side reaches the tier through `apply_quad_batch`. HDB-84's
appended-run rewrite never touched it. Both are measured below. **HDB-102 has
since closed this gap** — see "`apply_quad_batch` takes the append-run path"
below for the current numbers; everything in this section is the state before
it.

##### Bulk loader (`insert_quad_batch`) — `copy_forward` is gone, `merge_runs` replaced it

Append of 1,002,000 triples at the shipped 65,536-triple batch, into the loaded
9,995,000-triple store:

| phase | overlap | % | fresh | % |
|---|---|---|---|---|
| `merge_runs` | 0.952s | 54.4 | 0.974s | 52.7 |
| `build` | 0.020s | 1.1 | 0.022s | 1.2 |
| `group` | 0.020s | 1.1 | 0.021s | 1.1 |
| `copy_forward` | not emitted | 0 | not emitted | 0 |
| parse (measured separately) | 0.605s | 34.6 | 0.617s | 33.4 |
| interning (residual) | 0.153s | 8.7 | 0.216s | 11.7 |
| **append total** | **1.750s** | | **1.850s** | |

The loader has no `intern` counter, so the dictionary line is the residual
after the counted phases and a separately-measured parse of the same file. It
is a derived number, not a counted one, and the subtraction crosses two parse
pipelines: the parse figure comes from the driver's own `parse_file`, which
materialises a `Vec<(OxTerm, OxTerm, OxTerm)>` the bulk loader never builds. So
it over-subtracts, making the residual an *under*-estimate of interning. The
`HornBackend` `intern` row (0.189-0.201s for the same corpus) bounds the error
at well under 0.05s. See "What this does not settle".

`merge_runs` re-merged **9,122,000 rows to add 1,002,000** — the six touched
partitions in full. That is the successor to `copy_forward`: the carry is
deferred to the first read and paid once per partition version instead of once
per batch, but it is still O(partition), not O(batch). Chunking no longer
matters (one call for the whole append: 1.732s overlap / 1.818s fresh, within
the spread of the 16-call numbers), which is HDB-84 working as designed.

Throughput: the base loads at 1.01 M triples/s, the 10%-append at 0.57 M
triples/s. The gap is `merge_runs`, and it widens as the store grows.

##### SPARQL ingest (`apply_quad_batch`) — `copy_forward` is 30% and the rebuild is 94%

Same append through `HornBackend::insert_oxrdf_batch`, the path HDB-85's
empty-store table covers:

| phase | one call, overlap | % | 16 calls, overlap | % | 16 calls, fresh | % |
|---|---|---|---|---|---|---|
| `build` | 1.049s | 42.7 | 15.816s | 64.5 | 15.978s | 64.7 |
| `copy_forward` | 0.447s | 18.2 | 7.351s | 30.0 | 7.391s | 29.9 |
| `dedupe` | 0.527s | 21.5 | 0.563s | 2.3 | 0.550s | 2.2 |
| `intern` | 0.169s | 6.9 | 0.201s | 0.8 | 0.189s | 0.8 |
| `live_keys` | 0.111s | 4.5 | 0.173s | 0.7 | 0.167s | 0.7 |
| `group` | 0.077s | 3.1 | 0.057s | 0.2 | 0.056s | 0.2 |
| `merge` | 0.017s | 0.7 | 0.017s | 0.1 | 0.017s | 0.1 |
| `stage` | 0.021s | 0.9 | 0.016s | 0.1 | 0.016s | 0.1 |
| `invalidate` | 0.000s | 0.0 | 0.000s | 0.0 | 0.000s | 0.0 |
| **accounted** | **2.423s** | **98.7** | **24.248s** | **98.8** | **24.425s** | **98.8** |
| **append total** | **2.456s** | | **24.534s** | | **24.712s** | |

Phase names as of the measured commit: HDB-89 has since deleted the `live_keys`
row (0.7% here), and the counts this path returns now come from storage.

Every cell is the median of its own three reps, so a column need not add up to
its `accounted` median — the 16-call columns are 0.05s short of it, 0.2%. Each
individual run does add up exactly. `invalidate` is listed to make the phase
set complete; it clears a snapshot cache and costs under a millisecond.

Read the two right-hand blocks against the left one. **Appending 1M triples in
16 calls costs 24.5s — twice what loading the entire 10M base costs (12.3s),
and 10x what the same 1M costs in a single call.** `copy_forward` plus `build`
are 94% of it, and both are the same per-batch partition rebuild HDB-84 removed
from the other path: each of the 16 calls (15 full 65,536-triple chunks plus an
18,960 remainder) carried the whole partition forward (137,784,320 rows carried
to add 1,002,000) and rebuilt its columns.

`Store::apply_quads` on a bare `Store`, with no `HornBackend` above it, gives
the same tier numbers (`copy_forward` 7.51s, `build` 16.14s of a 24.26s
append), so this is the tier's cost, not the SPARQL layer's. HDB-102 removed
that cost: the same 16-call append is **1.446s** there now.

##### HDB-57 R7: confirmed

R7 predicted that measuring the persistent dictionary on an empty-store load
overstates its value, because on a reopen-and-append `copy_forward` would
dominate. Both halves hold, with one correction to the mechanism.

Dictionary work as a share of the load:

| load | dictionary work | share |
|---|---|---|
| empty store, 10M, `HornBackend` (HDB-90) | 4.77s of 23.74s | **20.1%** |
| append 1M, `HornBackend`, one call | `dedupe` + `intern` = 0.696s of 2.456s | **≤28%** (~17% real, see below) |
| append 1M, `HornBackend`, 16 calls | `dedupe` + `intern` = 0.764s of 24.534s | **≤3.1%** |
| append 1M, bulk loader, any chunking | 0.153–0.216s of 1.750–1.850s | **8.7–11.7%** |

(`dedupe` also contains `intra_batch.insert` and the term moves, so the
`HornBackend` rows are upper bounds on the dictionary's share, not its cost.)

**The one-call row needs its bound tightened before it reads as a
counter-example.** At ≤28% it sits *above* the empty-store 20.1% R7 calls
inflated. It is an upper bound over a phase that is only about half dictionary
work: HDB-90 measured `dedupe` as 46.7% interning, the rest `intra_batch`
inserts and term moves. Applying that split puts the real one-call share at
`0.527s x 0.467 + 0.169s = 0.415s of 2.456s` = **~16.9%**, below 20.1%. The
split was measured on an all-miss empty-store load, so it is imported, not
re-measured — treat ~17% as an estimate inside a 6.9-28% bracket. Even the
bracket's top is reached only by chunking the whole append into a single tier
call, which no real ingest does. The chunked rows are the ones to plan against.

The correction: on the path S2 should be graded against — the bulk loader —
`copy_forward` **is not emitted at all**, so R7's stated term is measured at
zero there. Its role is taken by `merge_runs`, at 52–54% of the append against
the dictionary's 9–12%. R7's conclusion survives the substitution intact, and
on the SPARQL ingest path `copy_forward` really was still there, at 30% — until
HDB-102 removed it from every add-only batch, after which that path emits none
either.

The vocabulary experiment settles a second question. Tripling the miss count
(167,000 → 501,426 new terms) moved the append by 0.10s on the loader path and
by 0.18s — under the noise of `copy_forward` — on the ingest path. Misses are
not where the dictionary's time goes on an append; the 2.84M probes are, and
almost all of them are hits. That is what R3's repeat cache targets, and it
argues for aiming S2's structure choice at hit latency, not at miss handling —
consistent with R4's rejection of the binary fuse filter, and it closes the
"append-a-new-dataset path is still unexamined" carve-out R4 left open.

##### The S2 acceptance bench, restated

SPEC-25 acceptance #2 currently reads as a *reopen* test: resolve the LUBM-100
dictionary both directions without re-interning, reopen time I/O-bound. Keep
that — it is the durability property. But it does not grade what R7 asked
about, and R8's ns/lookup matrix on its own will justify work the end-to-end
number cannot repay. Add:

1. **Grade the dictionary against the bulk-loader append, not an empty-store
   load.** Reopen a store holding LUBM-100, append 10% more, and report the
   dictionary's share of that append. The number to beat on trainmarks is
   **9–12%**; the empty-store 20% is the wrong target.
2. **State the ceiling next to the ns/lookup table.** A dictionary that costs
   nothing at all makes the measured append 9–12% faster on the loader path and
   ~3% faster on the chunked ingest path. R8's matrix should carry that ceiling
   so a 2x lookup win is not read as a 2x load win.
3. **Report hits and misses separately**, and weight them by the measured
   ~90% hit rate on an append (HDB-92 R9 F1, reconfirmed here: 2.84M probes,
   167K–501K misses). Miss cost is third-order.
4. **Fix the tier first.** On the ingest path the dictionary cannot be worth
   more than 3% until `apply_quad_batch` stops carrying the partition forward
   per batch (HDB-102). That is a tier task, not an S2 task, and it gates
   whether S2's load-path argument is worth making on that path at all.
   **HDB-102 has since done it** — the 16-call append is 1.446s through
   `HornBackend`. The 3% no longer holds against that denominator, so re-derive
   the share before quoting it. Watch which phase carries the dictionary on
   which path: `HornBackend` reaches the tier through `Store::insert_quad_ids`,
   so `LoadPhase::Intern` is zero there (HDB-87) and the dictionary work sits
   inside `dedupe` — 0.344s of 1.446s, an **upper bound** of 24%, or ~11%
   applying HDB-90's measured 46.7% interning split to `dedupe`. Only the
   term-based bare-`Store` path (`--path apply`) emits `intern`, at 0.223s of a
   1.258s append, **18%**.

##### What this does not settle

- **The bulk loader has no `intern` counter.** Its dictionary line above is
  wall minus counted phases minus a parse measured through a different
  pipeline, so it under-estimates. Good enough for a share (both derivations
  land within 0.05s of the `HornBackend` `intern` row), not good enough to plan
  against. Instrumenting `QuadSink` needs a per-chunk bracket, not a per-triple
  one.
- **`merge_runs` is one bracket over three jobs** — the merge sort, the column
  materialisation, and the object-major sort above `hot_threshold`. Coarse
  enough to name it the dominant term, too coarse to say which of the three to
  attack.
- **One corpus, one append ratio.** 10% into 10M on a 6-predicate append. The
  merge is O(partition), so both the loader's 0.57 M triples/s and the
  `merge_runs` share get worse as the base grows or the append shrinks;
  neither curve was measured. N-Triples only; Turtle differs in `parse` and in
  nothing else.
- **Only this summary survives.** The raw per-rep driver output was not kept.
  Re-derive it with the commands under *Reproducing the numbers* — the corpora
  are deterministic, one command per mode.
#### Cutting the `apply_quad_batch` hash tables (HDB-88, 2026-09-01)

`Tier::apply_quad_batch` is the tier entry point every SPARQL-side write takes
(`Store::insert_quads` is a wrapper over `apply_quads` — see the HDB-91 section
above). Three things in it were built on hash tables that did not earn their
keep:

1. `group` built `HashMap<GraphId, HashMap<TermId, HashSet<(u64, u64)>>>` for
   **both** sides of the batch — one hash insert per incoming quad, 10M of them
   into a handful of very large sets — purely to absorb in-batch duplicate
   targets.
2. The add pass then iterated that set, so rows reached the partition builder
   in **hash order** — the worst input a sort can get.
3. `still_visible` was a `HashSet` sized by the *existing* partition, one insert
   per live row carried forward.

The add side is now a `Vec`, sorted and deduplicated once per predicate at the
end of `group`. The sort does the same in-batch dedupe on a 16-byte element
instead of a hash table, and leaves the pairs in the order the builder wants.
`still_visible` becomes a sorted `Vec` — the rows already arrive in the
partition's SPO order, so it is a push per row — and the add pass walks it with
one merge cursor, `O(live rows + adds)`, never worse than the copy-forward pass
it sits beside. The del side stays a `HashSet`: it is *probed* once per live
row, which is a lookup, not an iteration. Finally `Columns::sort_dedup` skips
its sort outright when the rows already arrive sorted (`is_sorted_by` stops at
the first out-of-order pair, so unsorted input pays a few comparisons).

`hornbench` (Ryzen 7 7700, 16 threads, Debian 6.12, rustc 1.90.0), snmalloc,
serial parse, trainmarks xlarge (9,995,000 triples). Before `4459be9`, after
`c0b0b07`. Driver: `incremental_load --path insert`, three interleaved reps per
cell, median reported; run-to-run spread is under 2% on every cell. The driver
parses outside the timed window, so these walls are the tier-side insert alone,
not a whole load — the phase seconds are directly comparable to HDB-85's table.

##### Bulk insert: 10M into an empty store, one call

| phase | before (ttl) | after (ttl) | before (nt) | after (nt) |
|---|---|---|---|---|
| `group` | 1.382s | **0.240s** | 1.381s | **0.253s** |
| `build` | 1.208s | **0.934s** | 1.222s | **0.940s** |
| `merge` | 0.135s | 0.161s | 0.176s | 0.133s |
| `copy_forward` | ~0 | ~0 | ~0 | ~0 |
| `dedupe` (control, `HornBackend`) | 4.949s | 4.932s | 4.950s | 4.952s |
| **tier total** | **2.725s** | **1.335s** | **2.779s** | **1.326s** |
| **insert wall** | **7.696s** | **6.314s** | **7.782s** | **6.301s** |

`group` −83%, `build` −23%, the tier's own work **−51%**, and the insert stage
−18% end to end. `dedupe` is the control: it is `HornBackend`'s work, untouched
here, and it does not move.

**`group` got faster while doing a sort, because the corpus is already in
subject order.** trainmarks is generated subject-major and `HornBackend` feeds
document order, so within a predicate the pairs arrive sorted and
`sort_unstable` takes pdqsort's already-sorted path.

On a randomly ordered corpus `group` would pay a real sort. **Into an empty
partition that is a move, not an addition** — the builder then receives exactly
the sorted add list, so `Columns::sort_dedup` skips its own sort and the work
nets out. **On an append it is additive**, because the builder receives carried
rows followed by adds, which is two sorted runs rather than one sorted
sequence, so `sort_dedup` sorts the whole partition either way. The append case
still comes out ahead — the adds are small against the partition, and the
`still_visible` and memory wins do not depend on arrival order at all — but the
group-phase sort is genuinely extra there, not relocated.

##### Append: into the loaded 9,995,000-triple store

98,000 triples in one call, and 1,002,000 triples in 16 calls of 65,536 (the
shipped batch size) — the HDB-91 / HDB-102 path:

| phase | 98k, 1 call, before | after | 1M, 16 calls, before | after |
|---|---|---|---|---|
| `copy_forward` | 0.566s | **0.180s** | 7.393s | **2.032s** |
| `build` | 1.104s | 1.107s | 15.947s | **13.162s** |
| `group` | 0.005s | 0.002s | 0.058s | 0.021s |
| `merge` | 0.003s | 0.001s | 0.017s | 0.072s |
| **append wall** | **1.724s** | **1.334s** | **24.119s** | **15.981s** |

`copy_forward` −68% / −72%: that is the `still_visible` `HashSet` disappearing.
The 16-call append — the case HDB-91 measured at "twice what loading the whole
base costs" — drops **−34%**. It is still the per-batch partition rebuild, just
a third cheaper; HDB-102 removed the rebuild itself, taking the same append to
1.446s.

`merge` rises from 0.017s to 0.072s on the 16-call append. That is the merge
cursor scanning `still_visible` once per predicate per batch instead of hashing
each added pair. It is the `O(live rows + adds)` term, it is 0.45% of the
append, and it buys the 5.4s off `copy_forward`.

##### `hot_threshold` is now reachable, and the eager build buys nothing today

`DEFAULT_HOT_THRESHOLD` (1,000,000 live rows) decides whether a partition
materialises the object-major layout at build time or on the first
object-major read. It was settable only from Rust source. It now resolves once
per process from `HORNDB_HOT_THRESHOLD` (`<n>`, or `off` for "never eager"),
with `horndb_storage::set_hot_threshold` for code that wants to override the
environment — the same shape as `HORNDB_LOAD_THREADS`.

Same driver and corpus, after-commit only, `--base xlarge.ttl`. Two reps per
row, agreeing within 1%; rep 1 shown. (`bench-trainmarks --scale xlarge`
reproduces the query half of this sweep.)

| `HORNDB_HOT_THRESHOLD` | `build` | `group` | `merge` | insert wall |
|---|---|---|---|---|
| `0` (every predicate eager) | 0.953s | 0.241s | 0.159s | 6.304s |
| `1000000` (the default) | 0.937s | 0.238s | 0.152s | 6.283s |
| `off` (every predicate lazy) | **0.230s** | 0.239s | 0.154s | **5.601s** |

Two things fall out:

- **`0` and the default are nearly the same run.** trainmarks xlarge has 15
  predicates, and the seven largest (`rdf:type` at 1,445,000 rows plus six at
  1,335,000) clear 1,000,000 and carry 94.6% of the corpus. The default is
  already "almost everything eager" here; the 0.016s between the two columns is
  the object-major build for the remaining 540,000 rows.
- **The eager object-major sort is 0.71s of a 10M load — 76% of `build` after
  the change above.** It is the single largest remaining item in the tier's
  budget on this path.

And it currently buys nothing: **no crate above `horndb-storage` calls
`scan_predicate_ordered`, `ordered_predicate`, or `top_predicates`.** The
SPARQL executor builds its own snapshot orderings (`VecTripleSource`, see the
HDB-97 sections above); the tier's object-major columns have no reader on any
shipped path. The full trainmarks xlarge suite says the same thing from the
other side — same host, same commit, two reps each, mean shown:

| | `0` | `1000000` (default) | `off` |
|---|---|---|---|
| `read_turtle` (whole load) | 15.798s | 15.694s | **14.904s** |
| `read_ntriples` (whole load) | 12.392s | 12.525s | **11.659s** |
| `q1_count` | 0.404s | 0.406s | 0.402s |
| `q2_customer_orders` | 2.186s | 2.207s | 2.196s |
| `q3_join_3_entities` | 1.194s | 1.171s | 1.194s |
| `q4_optional_aggregation` | 2.889s | 2.988s | 2.902s |
| `q5_construct` | 0.373s | 0.378s | 0.384s |
| `q6_delete_insert` | 0.505s | 0.507s | 0.505s |

Load is 0.79s (Turtle) / 0.87s (N-Triples) cheaper with the layout off — the
same 0.71s plus its allocation — and **not one of the six queries moves outside
the ±2% run-to-run spread, in either direction.**

Flipping the default to `off` would therefore take ~0.8s off every 10M load for
no measured loss. It is not flipped here: it changes SPEC-02 F4's stated
behaviour, and eager materialisation is what SPEC-25 S5 tiering and any future
tier-side ordered reader will want. The default stays at 1,000,000 as a
*measured* choice rather than an inherited constant, and the flip is left as its
own decision with these numbers behind it.

##### Reproducing

```bash
cargo build --release -p horndb-bench-trainmarks --bin incremental_load
D=target/trainmarks/data
# bulk insert into an empty store: read the `base` stage
./target/release/incremental_load --base $D/xlarge.ttl --append $D/medium.nt \
    --path insert --batch 0
# append into the loaded store: read the `append` stage
./target/release/incremental_load --base $D/xlarge.nt \
    --append $D/append_overlap.nt --path insert --batch 65536
# the threshold sweep
HORNDB_HOT_THRESHOLD=off ./target/release/incremental_load \
    --base $D/xlarge.ttl --append $D/medium.nt --path insert --batch 0
```

#### `apply_quad_batch` takes the append-run path (HDB-102, 2026-09-01)

HDB-84 stopped `insert_quad_batch` rebuilding a partition per batch. HDB-91 then
measured what that left behind: `Tier::apply_quad_batch` — the entry point every
SPARQL-side write reaches, since `Store::insert_quads` wraps `Store::apply_quads`
(SPEC-28 S6) — still carried every existing row forward and re-materialised the
whole partition on every call. HDB-88 made that rebuild ~a third cheaper without
changing its shape. This removes it, for the case where it is removable.

`apply_quad_batch` now chooses **per predicate**:

- **no deletion targets this predicate** → append-run path. The pairs that are
  not already live become one extra sorted run; nothing already stored is read,
  copied, or re-sorted, and the merge happens on the first read. Covers every
  add-only batch, and the add-only predicates of a mixed batch.
- **this predicate has deletion targets** → the pre-existing rebuild. A delete
  end-stamps a row inside an immutable run that pinned readers share by `Arc`,
  so it cannot be written in place. Left as it was (see "What this does not
  settle").

`ApplyReport::inserted` stays exact — `Store::insert_quads` returns it and
`INSERT DATA` idempotency is decided by it — so the append path still tests each
added pair against what is live, through `PredicatePartition::mark_live`: one
galloping search per *unmerged run*, never a `cols()` call. Going through the
merged view would force the whole-partition merge on every write and hand the
O(existing) cost straight back.

`hornbench` (Ryzen 7 7700, 16 threads, Debian 6.12, rustc 1.90.0), snmalloc,
`HORNDB_LOAD_THREADS` at the shipped `auto` (8 here). Base: trainmarks
`xlarge.nt`, 9,995,000 triples, brought to a fully-merged state first. Append:
`append_overlap.nt`, 1,002,000 triples. Before `58c4ab4` (HDB-96, which carries
HDB-88), after `bced5e4`. Driver `incremental_load`; three interleaved reps per
cell, median reported, run-to-run spread under 1% on every cell except
`main`/`apply`/one-call (7%).

##### The 16-call append, `--path insert` (`HornBackend` → `apply_quad_batch`)

1,002,000 triples in 16 calls of 65,536 (15 full chunks plus an 18,960
remainder), into the loaded 9,995,000-triple store:

| phase | before | rows before | after | rows after |
|---|---|---|---|---|
| `build` | 13.364s | 138,786,320 | **0.019s** | 1,002,000 |
| `copy_forward` | 1.720s | 137,784,320 | **not emitted** | 0 |
| `merge_runs` | not emitted | 0 | **0.930s** | 9,122,000 |
| `dedupe` | 0.390s | 1,002,000 | 0.344s | 1,002,000 |
| `group` | 0.021s | 1,002,000 | 0.021s | 1,002,000 |
| `merge` | 0.073s | 1,002,000 | **0.004s** | 1,002,000 |
| **append wall** | **15.830s** | | **1.446s** | |

**−90.9%.** The 137,784,320 rows the 16 calls used to carry forward are gone;
what is left is the same 9,122,000-row `merge_runs` the bulk-loader path has
paid since HDB-84 — six touched partitions, merged once, on the first read.

##### Chunked now costs what one call costs

The stated goal was for the 16-call append to land within noise of its one-call
cost. Append wall, both paths, both chunkings:

| entry point | chunking | before | after |
|---|---|---|---|
| `insert` (`HornBackend`) | 16 × 65,536 | 15.830s | **1.446s** |
| `insert` (`HornBackend`) | one call | 1.398s | **1.354s** |
| `apply` (bare `Store`) | 16 × 65,536 | 15.641s | **1.258s** |
| `apply` (bare `Store`) | one call | 1.371s | **1.270s** |

On the bare `Store` the two chunkings are now indistinguishable (1.258s vs
1.270s, inside the spread). Through `HornBackend` the 16-call run is 6.8%
above its one-call run; that gap is not the tier — the tier phases are within
0.005s of each other — it is 16 × the per-call `HornBackend` overhead, and it
shows up as the 0.128s unaccounted residual.

Throughput on the SPARQL ingest path goes from **0.063 M triples/s to 0.69 M
triples/s** for a 10% append into a 10M store. HDB-91's 0.04 M/s figure for the
same case was measured before HDB-88.

Note also the *one-call* before column: 1.398s of which `build` was 0.869s over
9,122,000 rows. Even one call rebuilt the whole partition. After, that same work
is 0.923s of `merge_runs` — the cost did not vanish there, it moved to the first
read, which is where the bulk loader has had it since HDB-84.

##### What this does not settle

- **The probe's cost is data-dependent, and this corpus is its best case.**
  Every appended subject in trainmarks is a freshly-interned order IRI, so its
  dictionary id sorts above everything in the base: the first gallop into the
  base run runs off the end and every later target short-circuits. That is why
  `merge` reads 0.004s. A workload adding new objects to *existing* subjects
  interleaves instead, and each target then costs a real `log(rows/adds)`
  gallop plus a binary search — analytically a few hundred million probes for
  this shape, order 0.3s, still an order of magnitude under the 15.8s it
  replaces, but it was not measured. Galloping is bounded by the linear merge it
  degenerates to, so it can never be worse than a single pass over the run.
- **Batches that delete are unchanged.** A predicate with any deletion target
  still pays `copy_forward` + a whole-partition `build`. Making it cheaper means
  giving `Columns` a versioned per-row `end` column so a stamp can be written
  without rewriting the run — a redesign of the MVCC row representation, out of
  this task's scope. Nothing here forecloses it. `DELETE DATA`, `CLEAR`/`DROP`
  and `DELETE … INSERT … WHERE` are the affected shapes; none was measured.
- **`merge_runs` is now the whole story on this path**, at 64% of the append,
  and it is O(partition) per partition version — so the append/base ratio and
  the base size both move it, and neither curve was measured. It is one bracket
  over three jobs (merge sort, column materialisation, object-major sort above
  `hot_threshold`), as HDB-91 already noted.
- **One corpus, one append ratio, one vocabulary flavour** (`overlap`). The
  `fresh` flavour was not re-run; HDB-91 measured the two within 1% of each
  other on this path and nothing here touches interning.
- **The single-triple insert path gets no benefit, and was not measured.**
  `HornBackend::insert_oxrdf_in_graph` — the shape HTTP `INSERT DATA` takes,
  one triple at a time — short-circuits on `StoreSnapshot::contains_quad`,
  which goes through `PredicatePartition::cols` and therefore **merges the
  runs before every insert**. Runs never accumulate there, so the deferred
  merge is paid by the very next insert instead of by a later reader: the same
  work, moved from the writer's `build` to `merge_runs` one call later. It is
  not cheaper, and it is not obviously dearer either — both the old rebuild and
  the new run merge hand `Columns::sort_dedup` the same row order (existing
  rows in SPO order, then the new one), so the already-sorted skip fires or
  fails identically, and both materialise one transient `Vec<Row>` of the whole
  partition. This is reasoning, not measurement: the path was timed neither
  before nor after. It is a known Stage-1 limit either way — single-triple
  insert into a columnar partition is the wrong shape, batch it.

##### Reproducing

```bash
cargo build --release -p horndb-bench-trainmarks --bin incremental_load
python3 scripts/bench/trainmarks/generate_data.py          # base corpora
python3 scripts/bench/trainmarks/generate_append.py --mode overlap \
    --triples 1002000 --out data/append_overlap.nt
# 16 calls; --batch 0 for the one-call cell, --path apply for the bare Store
./target/release/incremental_load --base data/xlarge.nt \
    --append data/append_overlap.nt --path insert --batch 65536
```

#### Which structure backs the mapped dictionary base (HDB-93, 2026-09-01)

SPEC-25 §S2 leaves the base structure "settled by the implementation plan with
bench evidence". HDB-57 R3 recommends a **minimal perfect hash function (MPHF)
plus a 64-bit fingerprint array** for term → id and **front-coded sorted blocks
plus an offset table** for id → term; R4 rejects FST (a finite-state transducer,
the Lucene-style compressed string map the `fst` crate implements) and ART/HOT
radix trees. Those calls came from published numbers on other people's
workloads. This is the matrix HDB-57 R8 asked for, measured on RDF terms here.

**Headline: with the repeat cache in front, the base structure stops being a
latency decision and becomes a space and cold-start decision — and on those
two axes the matrix picks FST, not the MPHF.**

`hornbench` (Ryzen 7 7700, 16 threads, 124 GB, Debian 6.12, rustc 1.90.0),
commit `d9d357c`, median of 3 reps per cell with the min–max range. The reps of
one cell run back to back inside one process rather than interleaved across
cells — this is a microbenchmark of five structures in one binary, not an A/B
of two builds. Spread is under 2% on every cell but three. Driver:
`cargo run --release -p horndb-storage --example dict_base_bench`.

##### The key sets

"Dictionary key" means what `Dictionary::intern` keys on — the IRI text, the
blank-node label, or a literal's lexical form plus its language tag or datatype
IRI. Key sets and their document-order term streams come out of HDB-92's
`scripts/bench/corpus_term_stats.rs`, which grew an `--emit-keys` mode for this,
so both tools use one definition of a key.

| key set | distinct keys | key bytes | mean key | mean shared prefix over sorted keys | source |
|---|---:|---:|---:|---:|---|
| trainmarks xlarge | 1,919,818 | 72.1 MB | 37.5 B | 26.8 B | real corpus |
| LUBM-100 | 3,303,902 | 196.8 MB | 59.6 B | 56.5 B | real corpus (UBA 1.7, 13,880,276 triples) |
| LUBM-scaled 10M | 9,909,316 | 604.2 MB | 61.0 B | 57.9 B | LUBM-100 re-instantiated at 3× universities |
| LUBM-scaled 100M | 99,082,405 | 6.17 GB | 62.3 B | 59.2 B | LUBM-100 re-instantiated at 30× universities |

The two real sets reproduce HDB-92's counts exactly — 1,919,818 and 3,303,902
distinct terms, 93.30% and 92.07% repeat rate — which is the check that this is
the same measurement.

The 10M and 100M sets are **not synthesised text**. They are the real LUBM-100
key set re-instantiated at more universities: every LUBM key naming a university
carries it as `University{k}`, and rewriting `k` to `k + stride·r` for `r` in
`0..factor` gives the distinct-term set the real generator would emit for
`100·factor` universities, keeping the real per-department irregularity, the
real literal mix and the real length tail. The stride is one past the largest
university number *present*, not the university count: LUBM-100 names
universities well outside the 100 it generates (`undergraduateDegreeFrom`
reaches `University975`), so a stride of 100 makes copy 9 collide with copy 0.

**Where that scaling is not neutral.** The key *text* is real, but the copies
are exact replicas differing in one number, where a real LUBM-3000 would jitter
per-department counts per university. That flatters `fst` alone, because a
constant id delta factors onto the university-number transition and the suffix
subtree is shared across all 30 copies. It costs the FST 34% of its bytes —
1.02 B/key on the real LUBM-100 set, 0.67 on the replication — and moves
nothing else. Every FST size ratio in this record is therefore quoted at the
**real-corpus** rate, and the per-key-set table under *Space and build* shows
both. The latency columns are unaffected: no arm's ns/lookup reorders across the
four key sets.

Two probe streams, because they answer different questions.

- **Corpus stream** — the real document-order term stream, replayed against the
  scaled sets one university block at a time. This is what a reopen-and-load
  sees, and the only stream under which the repeat cache means anything. Its
  measured 4,096-entry 4-way repeat-cache hit rate is 90.1% at 100M and 80.0% on
  trainmarks, bracketing HDB-92 F4's 78.5–84.3%.
- **Uniform stream** — every probe an independent uniform draw over the whole
  key set. Nothing is cached, every probe is a genuine random access. The
  pessimistic bound.

Misses are an existing key with a `0x01` byte appended: that byte cannot occur
in a key, so non-membership is guaranteed, and the miss still sorts next to a
real key, which is the worst case for the sorted structures.

##### The arms

| arm | what it is | resident or mapped |
|---|---|---|
| `hashbrown` | `hashbrown::HashTable<u32>` over the key arena. R8's control | resident |
| `openaddr` | open-addressed table, 32-bit tag cached per slot, explicit prefetch. The batch-32 control — `hashbrown` exposes no prefetch hook | resident |
| `ptrhash` | PtrHash MPHF (`ptr_hash` 2.1.1, `default_balanced`) plus a 16-byte record per slot: 64-bit fingerprint + id. HDB-57 R3's recommendation | MPHF resident (0.33 B/key), records mapped |
| `frontcoded` | front-coded sorted blocks of 16, binary search over block heads | mapped |
| `fst` | `fst` 0.4.7 `Map` over the sorted keys | mapped |

##### Warm matrix at 100M keys — ns/lookup

5,000,000 probes, 3 reps, median (min–max). "cache" is the 4,096-entry 4-way
LRU repeat cache from HDB-57 R3/R9 F4, indexed and tagged by the full term hash.

Hits, corpus document-order stream (repeat-cache hit rate 90.1%):

| structure | single | batch-32 | single + cache |
|---|---:|---:|---:|
| `openaddr` (resident) | 11.71 (11.53–12.26) | **6.84 (6.73–7.12)** | 11.55 |
| `hashbrown` (resident) | 21.52 (20.85–22.98) | 21.28 (20.79–22.85) | 18.37 |
| `ptrhash` + fingerprint | 45.14 (34.25–54.19) | 21.05 (21.05–21.33) | **17.01** |
| `frontcoded` | 152.75 (148.71–229.96) | 201.63 | 40.87 |
| `fst` | 223.02 (222.94–224.49) | 223.52 | 28.85 |

Hits, uniform stream (repeat-cache hit rate 0.0%):

| structure | single | batch-32 | single + cache |
|---|---:|---:|---:|
| `openaddr` | 72.20 | **60.25** | 94.26 |
| `hashbrown` | 86.55 | 87.40 | 129.48 |
| `ptrhash` + fingerprint | 181.09 | 134.70 | 202.32 |
| `fst` | 689.05 | 681.75 | 691.77 |
| `frontcoded` | 1223.80 | 657.38 | 1231.85 |

Misses, uniform stream:

| structure | single | batch-32 |
|---|---:|---:|
| `hashbrown` | **25.71** | — |
| `ptrhash` + fingerprint | 156.65 | **31.81** |
| `openaddr` | 90.65 | 55.86 |
| `fst` | 488.62 | — |
| `frontcoded` | 1020.90 | 626.35 |

Two cells to read carefully. **`ptrhash/single/hit/verify` costs 463.00 ns on
the uniform stream against 181.09 for the fingerprint-only path**: comparing the
key bytes after a fingerprint match adds two more random accesses (offset table,
then arena). R3's design skips that compare and accepts a 2⁻⁶⁴ chance of
returning a wrong id, which is the right call — but the fingerprint is doing
real work, not saving a few nanoseconds. And **the repeat cache is not free
where it does not hit**: on the uniform stream it costs `hashbrown` 50% (86.55 →
129.48). It is only right on a workload with repeats, which HDB-92 says every
corpus has (89.8–93.3% of calls).

##### How single-lookup hit cost scales

ns/lookup, single, hits, corpus stream / uniform stream:

| structure | 1.9M | 3.3M | 9.9M | 99M |
|---|---|---|---|---|
| `openaddr` | 10.50 / 41.41 | 7.05 / 56.61 | 11.32 / 59.17 | 11.71 / 72.20 |
| `hashbrown` | 18.99 / 46.16 | 7.76 / 51.60 | 14.86 / 69.32 | 21.52 / 86.55 |
| `ptrhash` | 27.69 / 97.47 | 24.25 / 117.48 | 27.20 / 132.43 | 45.14 / 181.09 |
| `frontcoded` | 141.98 / 419.18 | 130.90 / 503.36 | 129.22 / 716.93 | 152.75 / 1223.80 |
| `fst` | 158.48 / 307.04 | 210.73 / 407.73 | 219.02 / 452.13 | 223.02 / 689.05 |

Nothing reorders as the key set grows 52×. The corpus-stream column barely
moves for any structure — locality, not size, is what it is measuring.

##### Cold page cache — the reopen case

The page cache is flushed and dropped (`sync; echo 3 > /proc/sys/vm/drop_caches`)
immediately before each mapped probe loop, from inside the bench process, so the
key set stays on the heap and only the structure under test is evicted. Every
probe that misses is then a real fault to disk. 200,000 probes, 3 drop-and-probe
reps, median (min–max). "first touch" is the same loop with a warm page cache
but no page-table entries yet; "steady" is the matrix figure above.

100M keys:

| structure | cold, corpus stream | cold, uniform | first touch | steady | mapped size |
|---|---:|---:|---:|---:|---:|
| `fst` | **313.8 (309.4–325.9)** | **886.8 (886.8–904.7)** | 218.2 | 223.0 | 66.1 MB |
| `frontcoded` | 1713.7 (1648.2–1729.6) | 4591.7 (4567.2–4610.2) | 228.9 | 152.8 | 1.36 GB |
| `ptrhash` + fingerprint | 3648.8 (3642.7–3670.5) | 4025.2 (3963.1–4050.7) | 215.1 | 45.1 | 1.59 GB |

10M keys:

| structure | cold, corpus stream | cold, uniform | first touch | steady |
|---|---:|---:|---:|---:|
| `fst` | **220.8 (220.6–221.4)** | **488.7 (486.2–489.7)** | 210.0 | 219.0 |
| `frontcoded` | 342.7 (337.7–343.4) | 1056.6 (1053.6–1060.1) | 144.3 | 129.2 |
| `ptrhash` + fingerprint | 384.5 (374.0–389.1) | 500.2 (490.6–506.8) | 53.1 | 27.2 |

**The cold column reverses the warm one, but read it as a one-time cost, not a
standing latency.** At 100M the MPHF plus fingerprint array is the fastest mapped
structure warm (45.1 ns) and the slowest cold (3,648.8 ns). That ratio is not a
per-lookup penalty a server keeps paying: a cold cell is one file read amortised
over 200,000 probes, and it falls as the probe count rises. Multiply it out
instead.

| structure | mapped size | cold excess over its own steady state, uniform stream | implied read rate |
|---|---:|---:|---:|
| `fst` | 66.1 MB | (886.8 − 689.1) ns × 200k = **0.040s** | 1.67 GB/s |
| `frontcoded` | 1.36 GB | (4591.7 − 1223.8) ns × 200k = **0.674s** | 2.02 GB/s |
| `ptrhash` + fingerprint | 1.59 GB | (4025.2 − 181.1) ns × 200k = **0.769s** | 2.06 GB/s |

The three implied rates agree to within 20%, which says what the cold column is
actually measuring: **the time to pull the structure off this host's disk once,
at roughly 2 GB/s.** It therefore tracks file size, it is bounded by it, and it
cannot exceed it however many probes follow. The whole cold argument for FST is
**~0.73s of one-time faults avoided** at 100M keys.

Against that, FST costs 11.8 ns more per base lookup than the MPHF behind the
repeat cache (28.85 vs 17.01). The MPHF repays 0.73s after roughly **62M base
lookups — about 620M dictionary calls** at the measured 90.1% cache hit rate.
**A reopen-and-reload never reaches that and FST wins outright; a long-lived
server does reach it, and there the MPHF is ahead.** S2 should choose knowing
which of the two it is optimising, and the record recommends FST because reopen
is the case SPEC-25 §S2 exists for.

Two properties of the cold measurement that bound how far these ratios travel:

- **The corpus-stream cold column exercises 1/30 of the key space.** The
  replayed stream switches university block every 1,000,000 occurrences, so all
  200,000 cold probes fall in copy 0 — 17,878 distinct keys drive them, a 91.1%
  repeat rate. That is prefix-clustered and favours the FST beyond the size
  effect. **The locality-neutral uniform column is the one to quote downstream:
  FST is 4.5× better cold than the MPHF there (886.8 vs 4,025.2), against 11.6×
  on the corpus stream.** Downstream documents carry 4.5×.
- The cold loop has no repeat cache in front of it, but the corpus stream is
  ~90% repeats regardless, so far fewer than 200,000 distinct pages are faulted —
  17,878 distinct keys at 100M. The per-lookup figure is the file read spread
  over the repeats, not one fault per probe.

The "first touch" column isolates a cost that is easy to miss: **215–229 ns/
lookup with the data already in RAM**, purely from populating page-table entries
for a multi-GB mapping. A reopen pays it whether or not the file is on disk.

##### Space and build

Per key at 100M. "stores keys" says whether the structure can answer a lookup
without a separate copy of the key bytes.

| structure | B/key | stores keys | resident or mapped |
|---|---:|---|---|
| `fst` map | **0.67** | yes | mapped (66.1 MB) |
| PtrHash MPHF, `default_compact` (multi-threaded build) | 0.27 | no | resident |
| PtrHash MPHF, `default_balanced` | 0.33 | no | resident |
| front-coded blocks | 13.72 | yes | mapped (1.36 GB) |
| fingerprint + id records | 16.00 | no | mapped (1.59 GB) |
| `hashbrown` table | 5.93 | no | resident |
| `openaddr` table | 10.84 | no | resident |
| flat arena + offset table | 70.28 | yes | mapped (6.57 GB) |

PtrHash's own size matches its published 2.4 bits/key closely — 2.609 bits/key
balanced, 2.143 bits/key compact, constant across all four key sets. The
fingerprint array beside it is 49× larger than the MPHF, and that array, not the
MPHF, is what a lookup touches.

**`fst` size is corpus-dependent, and the 100M point overstates it.** The
per-key-set rates, which the single-scale table above cannot show:

| key set | `fst` | front-coded | fingerprint + id records | flat arena + offsets |
|---|---:|---:|---:|---:|
| trainmarks xlarge, 1.9M (real) | 2.36 | 19.38 | 16.00 | 45.54 |
| LUBM-100, 3.3M (real) | **1.02** | 13.58 | 16.00 | 67.57 |
| LUBM-scaled 10M | 0.82 | 13.65 | 16.00 | 68.98 |
| LUBM-scaled 100M | 0.67 | 13.72 | 16.00 | 70.28 |

Only the FST rate moves with the scaling, and it moves the wrong way: **1.02
B/key on the real LUBM-100 set against 0.67 on its 30× replication, a 34% drop
from copying alone.** The replicated copies differ only in one number, so a
constant id delta factors onto the university-number transition and the suffix
subtree is shared across all 30 — exactly the mechanism the FST minimises best.
Front-coding, the records and the arena are all flat across the same scaling, so
the effect is specific to this arm.

**Quote the real-corpus rate: the FST is 15.7× smaller than the fingerprint
array on real LUBM-100 and 6.8× smaller on trainmarks, not the 24× the 100M
scale point suggests.** Downstream documents carry 15.7×. Even at the trainmarks
rate a 100M-key FST is 234 MB against the fingerprint array's 1.59 GB.

Build time, single invocation, from an in-memory key set:

| step | 10M keys | 100M keys |
|---|---:|---:|
| sort the keys (needed by `fst` and front-coding, not by the MPHF) | 1.65s | 26.69s |
| MPHF, `default_balanced` (1 thread) | 1.29s | 15.92s |
| MPHF, `default_compact` (16 threads) | **0.30s** | **3.22s** |
| fill and write the fingerprint + id records | 1.25s | 16.26s |
| build and write the front-coded base | 0.68s | 9.28s |
| build and write the `fst` map | 1.93s | 21.93s |
| every structure above, one process | 10.66s | 141.30s |
| peak RSS for that process | 1,210 MiB | 11,272 MiB |

**Checkpoint cost is seconds, not minutes, and the merge cadence does not need
to change.** A realistic base rebuild at 100M keys is 19.5s for the MPHF path
(compact MPHF 3.22s + records 16.26s, no sort needed) or 48.6s for the FST path
(sort 26.69s + FST 21.93s). If the design keeps front-coded id → term the sort
is paid either way, and the two paths are within 10% of each other.

##### id → term

R3 calls the front-coded base plus an offset table "the O(1) id → term probe".
It is O(1) *blocks*; inside the block it is a sequential decode of up to 16
front-coded entries. Measured at 100M, ns/lookup, corpus stream / uniform:

| structure | 100M | 10M | B/key |
|---|---|---|---:|
| flat offset table + mapped arena | **0.76 / 19.95** | 0.76 / 17.60 | 70.28 |
| front-coded blocks + id → rank table | 36.50 / 381.36 | 34.70 / 269.83 | 17.72 |

Front-coding costs **48× the latency to save 4.0× the bytes**. Both halves are
real; the plan should pick knowingly rather than assume front-coding is free.

##### The total mapped base, which is not 66 MB

The 15.7× term → id ratio above is one component. A base has to answer both
directions, and the id → term half dominates the footprint. Both recommendations
of this record, added up at 100M keys:

| design | term → id | id → term | total B/key | total at 99,082,405 keys |
|---|---:|---:|---:|---:|
| **recommended**: `fst` + flat offset table | 0.67 | 70.28 | **70.95** | **7.03 GB** |
| R3 as written: MPHF + fingerprint + flat offset table | 16.33 | 70.28 | 86.61 | 8.58 GB |
| `fst` + front-coded id → term | 0.67 | 17.72 | **18.39** | **1.82 GB** |
| R3 as written, front-coded id → term | 16.33 | 17.72 | 34.05 | 3.37 GB |

The MPHF rows use **16.33** B/key — the 16.00 B/key fingerprint + id records plus
the 0.33 B/key MPHF that has to be resident beside them — because a total budget
has to count both. The 15.7× ratio elsewhere in this record divides by **16.00**,
the mapped array alone, because that is the component being compared against the
mapped FST. Both are right for their own question; neither is a typo.

**Read the total, not the term → id column.** Choosing FST over MPHF plus
fingerprint shrinks the whole mapped base by **1.2×** if id → term stays a flat
offset table over the arena, or 1.9× if id → term is front-coded. The 15.7× is
real but applies to a component that is under 1% of the recommended design's
bytes. Anyone sizing an S2 base from this record should budget **~7 GB at 100M
keys**, not 66 MB.

##### Verdicts

**R1 — confirmed.** Probe count and locality dominate key length. Over one key
set with one length distribution, the structures differ 30× warm on nothing but
probes per lookup (1–2 hashing, ~23 for the front-coded binary search, one per
byte for the FST); and one structure varies 4× on nothing but locality (ptrhash
45.1 → 181.1 ns, corpus stream to uniform). Key length explains none of it.

**R3 term → id — replaced.** MPHF plus fingerprint is not the fastest term → id
structure at any scale measured: `openaddr` batched is 3.1× faster warm at 100M
(6.84 vs 21.05 ns) and `hashbrown` beats it on single lookups. Its real
advantages are that it stores no key bytes and that its miss path is cheap when
batched (31.81 ns). Against the two other *mapped* candidates it is the largest
(16.3 vs 13.7 vs 0.67 B/key) and the worst cold (3,648.8 vs 1,713.7 vs
313.8 ns). **Take FST for the mapped term → id base.** With the repeat cache in
front — which R3 already requires — FST costs 28.85 ns against the MPHF's 17.01,
a 11.8 ns difference on the ~10% of calls that reach the base at all, in
exchange for a term → id structure **15.7× smaller on real-corpus rates** (6.8×
on trainmarks), **4.5× better cold on the locality-neutral stream**, and ordered,
prefix and automaton search that SPEC-25 will want for `STRSTARTS` and
regex-over-dictionary. Two bounds on that trade, both from the sections above:
the whole mapped base shrinks **1.2×**, not 15.7×, because id → term dominates
the footprint; and the cold advantage is **~0.73s of one-time faults**, which the
MPHF repays after ~620M dictionary calls. Reopen never reaches that, a
long-lived server does.

**R3 id → term — confirmed with its cost named.** Front-coded blocks plus an
offset table work, and save 4.0× the bytes, but cost 48× the latency of a flat
offset table over a mapped arena (36.50 vs 0.76 ns at 100M). Not O(1) in
practice.

**R3 repeat cache — confirmed, and it is the largest lever in the dictionary.**
At its measured 90.1% hit rate on the 100M corpus stream it cuts `fst` 223.02 →
28.85 (7.7×), `frontcoded` 152.75 → 40.87 (3.7×) and `ptrhash` 45.14 → 17.01
(2.7×), and collapses the spread between the three mapped structures from 178 ns
to 24 ns. That collapse is the result that decides the shape of S2: put the
cache in first, and the base structure is chosen on size and cold start.

**R4's FST rejection — overturned.** R4 rejected FST as "the worst latency
profile of the three for point lookups on a multi-GB map". The premise is wrong:
the FST is not multi-GB. It is 66 MB for 99,082,405 LUBM keys and 4.5 MB for
1,919,818 trainmarks keys — **15.7× smaller than the fingerprint array at
real-corpus rates**, 6.8× on trainmarks (the 24× at the 100M scale point is
inflated by the replication; see *Space and build*). Warm, R4's latency claim
holds (223.0 vs 45.1 ns, and 28.85 vs 17.01 with the cache). Cold, it is
backwards: 886.8 vs 4,025.2 ns on the locality-neutral stream, a **4.5×** FST
advantage worth ~0.73s of one-time faults at 100M keys. For a structure whose
whole purpose is reopen, that is the column that decides — and the reversal
survives at the conservative ratio, which is why the rejection does not.

**R4's ART/HOT rejection — still unmeasured.** No ART or HOT arm was built, so
this spike does not confirm it. What it does show is that R4's stated
mechanism — 3–6 *dependent* random accesses per lookup — is what dominates:
the front-coded arm's ~23 dependent probes cost 152.8 ns warm and 1,713.7 ns
cold at 100M, against 1–2 probes for the hash-shaped arms. The reasoning is
supported; the measurement R4 was asked for is not in hand. Filed as HDB-103,
which reuses this harness and these key sets, rather than claimed here.

**R5 batch-32 with software prefetch — confirmed for hash-shaped probes only.**
At 100M, corpus stream: `openaddr` 11.71 → 6.84 (1.7×), `ptrhash` 45.14 → 21.05
(2.1×), `ptrhash` misses 156.65 → 31.81 (4.9×). Caveat on the `ptrhash` ratio:
`ptrhash/single/hit` on that stream is the one noisy cell in the matrix, 45.14
over a 34.25–54.19 range (±22% on 3 reps), so its batching ratio spans
1.6–2.6×, and the 81× cold-to-warm figure quoted above spans 67–107×. Neither
range changes a verdict, but do not quote either ratio to three digits.

It does nothing for `hashbrown` (21.52 → 21.28 — the crate exposes no prefetch
hook), nothing for `fst` (223.02 → 223.52), and on the locality-rich corpus
stream it makes the front-coded base *worse* (152.75 → 201.63) while helping it
on the uniform stream (1223.80 → 657.38). Batching pays where the lookup is one
or two independent random probes and the queries are independent; it does not
rescue a dependent chain.

##### What a win here is worth end to end

HDB-91 measured the dictionary at **9–12% of a bulk-loader append** (1.85s for
1,002,000 triples over a 10M-triple base, ~2.84M dictionary probes). Applying
this matrix's spread to that:

- With the repeat cache, best to worst mapped structure at 100M is 17.01 →
  40.87 ns. Over 2.84M probes that is **0.068s, 3.7% of the append**.
- Without the cache the spread is 45.14 → 223.02 ns, i.e. 0.51s, 27% of the
  append.

So the cache is where the load-path money is, and the base structure is a
single-digit-percent decision on top of a 9–12% share. Do not present ns/lookup
as the bottom line. The reason S2 exists is that a reopened store must resolve
both directions without re-interning the corpus — LUBM-100's 3.3M distinct terms
come out of a 2.4 GB N-Triples document, and skipping that parse is worth far
more than any cell in this table. The cold column is the one that speaks to it.

**Scope of the bits/key numbers.** They are measured against the *current* key
encoding, where a typed literal's key is its lexical form plus a NUL plus the
full datatype IRI. HDB-95 proposes keying typed literals on
`(lexical, datatype-id)`, which HDB-92 F3 sizes at roughly 80% of that column's
key bytes. Do not grade HDB-95 against a baseline that already assumes its own
change.

##### Reproducing

```bash
# 1. corpus -> distinct keys + document-order term stream
rustc --edition 2021 -O -o /tmp/cts scripts/bench/corpus_term_stats.rs
/tmp/cts --name lubm-100 --emit-keys keys/lubm100 tbox.nt abox.nt

# 2. re-instantiate the real key set at 30x universities (~100M keys)
cargo run --release -p horndb-storage --example dict_base_bench -- \
  scale --src keys/lubm100 --dir keys/lubm-100M --factor 30 --keys 100000000

# 3. build every structure; prints build time and bits/key
cargo run --release -p horndb-storage --example dict_base_bench -- \
  build --dir keys/lubm-100M

# 4. the warm matrix
cargo run --release -p horndb-storage --example dict_base_bench -- \
  query --dir keys/lubm-100M --probes 5000000 --reps 3

# 5. one cold cell (needs passwordless sudo for drop_caches)
cargo run --release -p horndb-storage --example dict_base_bench -- \
  cold --dir keys/lubm-100M --arm fst --probes 200000 --drop-caches

# 6. the same loop without the drop -- the "first touch" column
cargo run --release -p horndb-storage --example dict_base_bench -- \
  cold --dir keys/lubm-100M --arm fst --probes 200000

# add --zipf -1 to either cold command for the uniform-stream column
```

**The probe counts are part of the result.** A cold cell is one file read
amortised over the probe count, so raising `--probes` lowers the cold ns/lookup
without anything having changed. Use 5,000,000 for `query` and 200,000 for
`cold` to reproduce the tables above. The example's module doc carries the same
values.

`ptr_hash`, `fst`, `hashbrown` and `memmap2` are dev-dependencies of
`horndb-storage`, reachable only from this example. They stay for as long as the
S2 plan cites this record; if S2 adopts a structure they should become real
dependencies of the crate, and if it adopts none of them they go with the
example.

#### The dictionary key carried the datatype IRI on every typed literal (HDB-95, 2026-09-01)

The forward map was `DashMap<Term, TermId>`, so a typed literal's key was the
whole term — lexical form **plus** datatype IRI. A corpus draws its datatypes
from a set of a few dozen, so that IRI was stored, hashed and compared millions
of times over. The key is now a compact byte string that carries a small dense
id for the datatype IRI (and for a language tag) instead of its text; the ids
live in two side tables private to the dictionary, so `TermId` assignment is
unchanged.

Measured with the HDB-92 term-stream analyzer (`scripts/bench/corpus_term_stats.rs`),
which now reports both keyings from a single pass. Re-keying is injective, so
the distinct-term count is identical on both sides by construction — the tables
below compare the *same* term sets.

The analyzer was later corrected to classify an explicit `"x"^^xsd:string` as a
plain literal, the way `kind_of` does (the engine's key carries no datatype for
it). Re-running all three corpora reproduced every figure below **byte for
byte** — none of them contains that shape — so the numbers stand and the tool is
now right for corpora that do.

**Typed-literal column, bytes per key.** Distinct-weighted is what the
dictionary stores; occurrence-weighted is what it hashes and compares.

| Corpus | Triples | Distinct typed literals | Distinct: before → after | Δ | Per occurrence: before → after | Δ |
|---|---:|---:|---|---:|---|---:|
| trainmarks xlarge | 9,995,000 | 464,182 | 46.70 → 7.71 B | **−83.5%** | 47.37 → 9.40 B | **−80.2%** |
| LUBM-100 | 13,880,276 | 0 | — | — | — | — |
| DBpedia infobox-properties EN (2016-10) | 52,680,098 | 9,735,120 | 87.93 → 36.12 B | **−58.9%** | 68.64 → 18.56 B | **−73.0%** |

**Whole dictionary, all kinds.** IRIs, blank nodes and plain literals keep
their bytes, so the whole-dictionary effect is the typed-literal saving diluted
by that share.

| Corpus | Distinct terms | Key bytes before → after | Δ | B/key before → after | Distinct datatypes / languages |
|---|---:|---|---:|---|---:|
| trainmarks xlarge | 1,919,818 | 72,064,576 → 53,965,510 | **−25.1%** | 37.54 → 28.11 B | 2 / 0 |
| LUBM-100 | 3,303,902 | 196,819,651 → 196,819,651 | **0%** | 59.57 → 59.57 B | 0 / 0 |
| DBpedia infobox-properties EN | 14,222,284 | 1,082,846,487 → 578,451,551 | **−46.6%** | 76.14 → 40.67 B | 243 / 0 |

**The saving is smaller on real data than the ~80% the task assumed, and for a
concrete reason.** On DBpedia the distinct typed literals carry a 35.1 B mean
lexical form — free text, not short numbers — so the datatype IRI is a smaller
share of the key than the 47.5 B / 8.2 B figure the task quoted implied.
Per *occurrence* it is 73%, because the long lexical forms are the ones that
repeat least. trainmarks, whose typed literals are short dates and decimals,
does hit the ~80%.

**Language tags were re-keyed the same way, and it is nearly free either way.**
None of the three corpora contains a language-tagged literal (DBpedia
infobox-properties spells `rdf:langString` values as typed literals with no
tag, which the analyzer and the engine both count as typed). Substituting a
one-byte id for a 2–5 byte tag would save 1–4 B on such a key. It is in scope
because it is the same code path and the same table type — not because the
evidence demands it.

**End-to-end A/B on the loader.** `hornbench` (Ryzen 7 7700, 16 cores, Debian
6.12, rustc 1.90.0, 2026-09-01), trainmarks xlarge N-Triples (9,995,000
triples), `examples/load_curve` at the shipped 65,536-triple batch, one process
per run, base and change **interleaved** across 5 reps. `ready` = load plus a
first read. Peak RSS via `getrusage(RUSAGE_CHILDREN)`.

| Commit | `ready` median | spread | Peak RSS median | spread |
|---|---:|---|---:|---|
| `902cb1e` (base) | 9.752 s | 9.709–9.872 s | 1,547.4 MiB | 1,538.2–1,550.5 |
| `ca4933e` (HDB-95) | **9.574 s** | 9.561–9.592 s | **1,337.8 MiB** | 1,334.3–1,349.3 |

**−1.8% load time (1.025 → 1.044 M triples/s) and −209.6 MiB peak RSS (−13.5%).**
The memory drop is ten times the 18 MB of key bytes the analyzer predicts,
because the forward map no longer stores an `oxrdf::Term` per slot: a ~64 B
inline enum with one or two separate `String` allocations behind it becomes a
16 B `Box<[u8]>` with one. Over 1.92M live keys plus hash-table growth headroom
that dominates the key bytes themselves.

The change's own instrumentation agrees with the offline analyzer to the byte:
`load_curve` reports **55,874,725 key bytes over 1,919,818 keys (29.10 B/key)**,
which is the analyzer's 53,965,510 plus the one-byte kind tag on every key,
minus the separator the analyzer already counted on plain literals.

##### What this does not settle

- **LUBM is untouched.** It has no typed and no language-tagged literals, so
  the key set is byte-identical. Any dictionary base-structure measurement
  taken on LUBM keys (HDB-93) still holds; the same measurement taken on
  trainmarks keys is now over a 25% smaller key set and has to be re-run.
- **Read-path effect unmeasured.** The A/B is the load path. `Dictionary::get`
  hashes a shorter key too, but no query bench was run for this change.
- **One-entry memo, not a cache.** The side-table lookup is answered from a
  thread-local one-entry memo, which suits corpora whose datatypes arrive in
  runs. A corpus that alternates between many datatypes term by term would fall
  through to the side-table probe on every typed literal; none of the three
  measured corpora does.
#### The parse-thread default flips to `auto` (HDB-96, 2026-09-01)

Every thread sweep before this one measured the trainmarks driver's path —
parse into one `Vec<Triple>`, then one `HornBackend::insert_oxrdf_batch`. The
default stayed at `HORNDB_LOAD_THREADS=1` because that is not a `Store` load,
and the reason HDB-83 gave for serial was the *tier* leg: `insert_quad_batch`
ran on the calling thread and had to free terms allocated on 16 parse threads
while rebuilding every partition the batch touched (40.1s → 46.6s).

This is that sweep, against the real thing.

`hornbench` (Ryzen 7 7700, 8 cores / 16 threads, 124 GB, Debian 6.12, rustc
1.90.0), commit `c6da644`, snmalloc, trainmarks xlarge (9,995,000 triples)
into a **fresh in-memory `Store`** through `load_turtle_slice_with_threads` /
`load_ntriples_slice_with_threads`, plus a first read to force the merge HDB-84
defers — a load nobody reads has work outstanding. Median of 3 runs; the reps
are interleaved across thread counts and formats, so any drift in host state
spreads over the whole table rather than favouring one cell. Host quiet (load
average 0.21 at start). Driver:
`cargo run --release -p horndb-bench-trainmarks --bin store_load -- --file <f> --threads <n>`.

The measured commit carries HDB-84 (`7d51a70`), HDB-87 (`02fe5b6`), HDB-91
(`902cb1e`), HDB-94 and snmalloc. It does **not** carry HDB-89 (`0bdf892`),
which merged while this sweep was running and is in the branch's final base.
That does not affect the numbers: HDB-89 deletes `live_keys` from
`HornBackend`, touches no file under `crates/storage/src/loader/`, and the
tables below show no `live_keys` or `dedupe*` row at all — the storage bulk-load
path never reaches that code. Its 1.81 GiB peak-RSS win is on the `HornBackend`
path, so the RSS column here is unaffected either way.

It also predates **HDB-95** (`4459be9`, the section above), which merged after
the sweep and shrank the dictionary key. That one *is* on this path — it is what
the `intern` row below measures — so read these numbers with its measured effect
applied: **−1.8% load time and −209.6 MiB peak RSS** at one thread. Too small to
change the decision or the shape of the curve (1.8% off the whole load takes the
Turtle `intern` row from 3.105s to about 3.05s, and its share from 56% to 55%),
but it does mean the absolute RSS figures below sit ~200 MiB high and the
+78% / +57% deltas a little wide. The sweep was not re-run across HDB-95.

Peak RSS is `VmHWM` read at process exit, one process per cell. The 386 MB /
1.17 GB source document is in memory in every cell (the driver reads it before
the timer starts), so it is a constant, not part of the delta.

**Turtle** (`xlarge.ttl`, 386,628,639 bytes):

| threads | wall | vs 1 | `parse` | `intern`~ | `group` | `build` | `merge_runs` | peak RSS |
|---|---|---|---|---|---|---|---|---|
| 1 | **12.926s** | — | 8.479s | 2.778s | 0.233s | 0.336s | 1.084s | 2,207 MiB |
| 2 | 7.388s | 1.75× | 2.613s | 3.124s | 0.239s | 0.335s | 1.081s | 3,210 MiB |
| 4 | 6.255s | 2.07× | 1.535s | 3.060s | 0.237s | 0.331s | 1.080s | 3,698 MiB |
| **8 (new default)** | **5.581s** | **2.32×** | 0.780s | 3.105s | 0.251s | 0.351s | 1.077s | 3,851 MiB |
| 16 | 5.499s | 2.35× | 0.777s | 3.043s | 0.264s | 0.360s | 1.075s | 3,915 MiB |

**N-Triples** (`xlarge.nt`, 1,168,988,375 bytes):

| threads | wall | vs 1 | `parse` | `intern`~ | `group` | `build` | `merge_runs` | peak RSS |
|---|---|---|---|---|---|---|---|---|
| 1 | **9.672s** | — | 5.290s | 2.749s | 0.229s | 0.333s | 1.080s | 2,940 MiB |
| 2 | 5.926s | 1.63× | 1.223s | 3.026s | 0.241s | 0.338s | 1.094s | 4,012 MiB |
| 4 | 5.199s | 1.86× | 0.561s | 2.971s | 0.239s | 0.334s | 1.081s | 4,436 MiB |
| **8 (new default)** | **4.903s** | **1.97×** | 0.176s | 3.012s | 0.251s | 0.352s | 1.092s | 4,612 MiB |
| 16 | 4.956s | 1.95× | 0.352s | 2.916s | 0.260s | 0.351s | 1.095s | 4,699 MiB |

Run spread is under 1.6% on every cell (widest: Turtle at 1 thread, 12.630 /
12.926 / 13.038s). No two thread counts overlap except 8 against 16.

`intern` is not a counter — the storage load path does not instrument it — so
it is reported as the residue after `parse` and the tier phases. `parse` is
newly emitted by the slice loaders and is taken once per 8,192-item batch: the
calling thread's wall clock minus the time it spent in the sink. At one thread
that is the inline parse; above one it is what the consumer still waits for
after the parse threads have run ahead.

**This flip moves the bottleneck to `intern`**, which is now 56% of a Turtle
load and 61% of an N-Triples one — larger than `parse` and the tier together,
and still the largest phase after HDB-95 took 1.8% off the whole load.
It is serial by construction: interning runs on the calling thread in document
order so term ids do not depend on thread scheduling, which is what makes a
parallel load produce a byte-identical store (pinned by
`parallel_loader.rs::assert_same_store`, which compares term ids). That is a
property with a cost, not an oversight, and attacking it naively would break it.
Instrumenting and addressing it is **HDB-106**.

**HDB-83's reason for the serial default is gone.** The tier phases are flat in
the thread count — `group` + `build` + `merge_runs` total **1.65s at 1 thread
and 1.70s at 16** on Turtle, a 3% drift over a 16× change in producers. HDB-84
replaced the per-batch partition rebuild with an appended run, so the work that
used to be serialised behind 16 allocating parse threads is no longer done on
the write path at all.

What survives of the cross-thread free cost is small and lands on interning,
not the tier: `intern` rises **2.78s → 3.05s (+10%)** from 1 to 2 threads and is
then flat to 16. That is the whole of the effect HDB-83 measured as a 16%
end-to-end loss.

**Why the cap is 8 and not `available_parallelism()`.** The sweep flattens at 8.
The 16th thread is worth 1.5% on Turtle and **−1.1% on N-Triples** — the two
disagree on its sign, which is the definition of noise — while costing another
64–87 MiB of peak RSS. It is not a scheduling artefact of this host having 8
physical cores and 16 SMT threads; the phase split says why directly. At 8
threads `parse` is already 14% of a Turtle load and 3.6% of an N-Triples one,
so driving it to *zero* could buy at most another 14%. The remaining 4.9s is
interning (3.0s) and the tier (1.7s), both of which run on the calling thread
by construction — interning stays there so term ids do not depend on thread
scheduling, which is what makes the parallel and serial paths produce
byte-identical stores.

The cap is also a guard on a default nobody sets: uncapped, a 64-core host
would spawn 64 parse threads for a leg that stopped scaling at 8 and pay for
all of them in per-thread parser state and scheduler pressure. An explicit
`HORNDB_LOAD_THREADS=<n>` is **not** capped — that is the escape hatch, and how
this table was taken.

**What it costs: memory, in two parts.** The tables above measure the first
part. Peak RSS rises 78% on Turtle (2,207 → 3,851 MiB) and 57% on N-Triples
(2,940 → 4,612 MiB). Almost all of that is the 8M-triple in-flight parse budget
from HDB-94, which a one-chunk load never allocates at all; the rest is
per-thread parser and allocator state. That budget is absolute — a fixed
~1.5 GiB adder, not something that grows with the corpus — and
`HORNDB_LOAD_BUFFER_TRIPLES` trades it back against `parse`. It was tuned by
HDB-94 at 16 threads on the `HornBackend` path, where `parse` dominated; at the
shipped 8 threads on this path `parse` is 14% / 3.6%, so a smaller budget
plausibly keeps most of the speedup for a fraction of the memory. Unmeasured —
**HDB-105**.

**The second part is not in the tables, and it does scale with the document.**
The parallel path needs one contiguous slice, so the *file* entry points
(`load_ntriples_file`, `load_nquads_file`, and `load_turtle_file` under
`HORNDB_PARALLEL_TURTLE=1`) reach it by reading the whole document into memory,
where the streaming path holds a 1 MiB `BufReader`. That branch already existed;
it was unreachable by default because it needs `threads > 1`. **Flipping the
default makes it the default**, so the real cost at those entry points is
`file size + ~1.5 GiB`, and file size is unbounded. The sweep cannot show this:
`store_load` reads the document before the timer starts and calls the slice
functions directly, so its RSS column deliberately holds the document constant
across cells.

Left alone, that turns "slower" into "fatal" — a document larger than RAM loaded
before HDB-96 and would OOM after it, on the path `crates/bench-rdfox` and the
unmeasured SPEC-02 LUBM-8000 gate (~1B triples) both use. So the flip ships with
a ceiling: `HORNDB_LOAD_MAX_SLICE_BYTES`, default **2 GiB**
(`loader::max_slice_bytes`). Above it the file loaders fall back to the
streaming reader on one thread — exactly what they did before HDB-96 — so a
large file loses the speedup and keeps the flat footprint.

**Why 2 GiB.** Not the document-to-store size ratio: that is roughly
scale-invariant for RDF, so it does not invert at any threshold. Two other
reasons carry it. First, the ceiling makes the transient **absolute** — whatever
the corpus, a threaded file load can exceed a streaming one by at most the
ceiling plus the parse budget, worst case about **+3.5 GiB**, and any host with
room for the store being built has room for that. Without a ceiling the term is
unbounded, which is the hazard. Second, what tripping it forgoes is small: at 8
threads `parse` is 14% / 3.6% of the load. For scale, trainmarks xlarge is a
1.17 GB document and a ~1.7 GiB store, so 2 GiB sits just above a corpus of that
size — inside the bound, not fitted to it.

Measured on the file path, which is the one the sweep tables cannot show.
`examples/load_curve`, `xlarge.nt` (1.17 GB), one repeat per cell, `VmHWM`
sampled at 200 Hz by a separate watcher — the sampling costs ~5% wall, so read
the times as shape, not as headline numbers:

| `load_ntriples_file` path | peak RSS | ready |
|---|---|---|
| default: 8 threads, document read whole | **4,029 MiB** | 6.52s |
| `HORNDB_LOAD_MAX_SLICE_BYTES=1000000000` → streaming fallback | **1,544 MiB** | 10.34s |
| `HORNDB_LOAD_THREADS=1` → streaming | **1,544 MiB** | 10.11s |

The threaded file load costs **2,485 MiB over streaming**, of which ~1.17 GB is
the document itself — that is the term "fixed adder" got wrong. And the ceiling
fallback lands on **exactly** the serial footprint (1,544 MiB, to the MiB),
which is the point: above the ceiling you get the pre-HDB-96 behaviour, not an
intermediate one. Both fallback rows load all 9,995,000 triples.

The `load_curve` figures below are inside the ceiling (1.17 GB < 2 GiB), so they
are unaffected by it — the ceiling was not chosen to clear them, and a corpus
above it would simply have reported the one-thread number. Re-checked after the
ceiling landed: 6.130s median, against the 6.091s recorded below.

`HORNDB_LOAD_THREADS=1` restores the old time *and* both parts of the old
footprint.

**Contention was checked, not assumed.** HDB-84 introduced a reader-blocks-
writer path — `PredicatePartition::cols()` holds the `runs` mutex across the
merge, and hitting `MAX_RUNS` forces a merge on the write path — which did not
exist when HDB-83 measured. Neither fires here. The load is single-consumer:
the parse threads never touch the tier, and the only reader is the first read
after the load completes, which is where the single `merge_runs` sample comes
from (1.08s, unchanged across every cell). The cap is not approached either:
9,995,000 triples in 65,536-triple batches is 153 runs against a cap of 4,096.
Flat `merge_runs` across the sweep is the evidence for both.

**Turtle *files* do not change.** `load_turtle_file` needs
`HORNDB_PARALLEL_TURTLE=1` as well, because splitting Turtle carries a
soundness caveat the line-based formats do not. The flip reaches
`load_ntriples_file` / `load_nquads_file` and every direct `load_*_slice`
caller.

**End-to-end on the F8 driver.** `examples/load_curve` at the shipped
65,536-triple batch, same host and commit, load plus the first read, three
medians of three:

| | median "ready" | triples/s |
|---|---|---|
| `HORNDB_LOAD_THREADS=1` | 9.486 / 9.478 / 9.520s | 1.05 M/s |
| shipped default (`auto` → 8) | 6.091 / 6.085 / 6.107s | **1.64 M/s** |

That is the F8 row above, restated: this driver includes the 1.17 GB file read
inside the timer, which `store_load` does not, so its numbers sit ~1.2s above
the sweep's.

**One comparability note.** `bench-trainmarks` picks its parse thread count
from `load_threads()`, so the trainmarks `read_turtle` / `read_ntriples` legs
now default to 8 threads too. Every trainmarks number recorded before this
commit was taken at one parse thread; pass `HORNDB_LOAD_THREADS=1` to reproduce
them.

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

# SPEC-25 S2 — dictionary base-structure matrix (HDB-93). Not criterion: a
# spike driver with its own modes. Full protocol and the probe counts the
# published tables use are in "Which structure backs the mapped dictionary
# base (HDB-93)" above.
cargo run --release -p horndb-storage --example dict_base_bench -- \
  query --dir keys/lubm-100M --probes 5000000 --reps 3

# SPEC-02 F8 — bulk-load wall time vs tier batch size (HDB-84). Not criterion:
# one load per invocation, so sweep the batch size yourself. 0 = one insert call.
cargo run --release -p horndb-storage --example load_curve -- data/xlarge.nt 65536 3

# Phase table for a load into a non-empty store (HDB-91). Generate the two
# append corpora once, then run one process per cell. --path selects the tier
# entry point: load = bulk loader (insert_quad_batch), insert = HornBackend,
# apply = Store::apply_quads (both apply_quad_batch).
python3 scripts/bench/trainmarks/generate_append.py --mode overlap \
    --triples 1002000 --out data/append_overlap.nt
python3 scripts/bench/trainmarks/generate_append.py --mode fresh \
    --triples 1002000 --out data/append_fresh.nt
cargo run --release -p horndb-bench-trainmarks --bin incremental_load -- \
    --base data/xlarge.nt --append data/append_overlap.nt --path load --batch 65536

# Dictionary key bytes per corpus, before and after the HDB-95 re-keying
# (offline; no engine build). Standalone rustc program, edition 2021 required.
rustc --edition 2021 -O -o /tmp/cts scripts/bench/corpus_term_stats.rs
/tmp/cts --name trainmarks-xlarge data/xlarge.nt > tm.json 2> tm.txt
bzip2 -dc infobox_properties_en.ttl.bz2 | /tmp/cts --name dbpedia - > dbp.json 2> dbp.txt

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
