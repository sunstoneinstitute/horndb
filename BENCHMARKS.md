# Benchmarks

Where we are, where we need to be, and what we measure against. This document is the single source of truth for the project's quantitative goals. Targets come from the per-subsystem SPECs (non-functional requirements and acceptance criteria); baselines come from the cited literature and vendor publications.

> **Status — 2026-06-17.** Stage 1 is in flight. Four numbers are now measured against the canonical Stage-1 benches — the WCOJ 4-cycle acceptance gate, the WCOJ differential fuzzer, incremental closure append, and the owlrl closure-backend A/B (all in the *Measured* table below); the rest of this document is targets and baselines that the harness (SPEC-01) will fill in as the engine grows. Live gaps are tracked in [`TASKS.md`](TASKS.md).

## Reference hardware

The "reference workstation" referenced throughout the SPECs:

- **CPU:** single AMD EPYC 9354 (Zen 4, 32C/64T)
- **DRAM:** 12-channel DDR5-4800
- **Storage:** local NVMe (HDT cold tier; SPEC-02)
- **Stage 3 only — accelerators:** AMD MI300A (preferred for unified HBM + Zen4) or NVIDIA GH200 / GB200

The harness captures a hardware fingerprint per run; comparisons are valid only within identical fingerprints (SPEC-01 NF — "we normalise by capturing the fingerprint, not by trying to normalise across hardware").

**Developer test-loop runner.** The integration test suite runs under
`cargo nextest run` (parallel across the ~90 test binaries, vs cargo's serial
per-binary default — [#108](https://github.com/sunstoneinstitute/horndb/issues/108)).
This is a dev-loop wall-clock concern, not a product performance gate, so its
before/after numbers are not tracked as authoritative rows here; the decision
and a rough local measurement live in `docs/architecture.md` → "Integration-test
runner". Any number recorded here as authoritative must still come from
`hornbench`.

**Where benchmarks are run.** All `cargo bench` runs that produce numbers
recorded in this document are executed on the dedicated **`hornbench`** server
(`ssh hornbench`; repo at `~/src/horndb`), *not* on a laptop — this keeps the
environment stable and comparable over time (and avoids laptop thermals/battery
throttling the result). `git fetch`/`pull` and check out the commit under test on
hornbench (or `rsync` over uncommitted files), run the bench there, and record
the numbers with their environment. Numbers measured on a laptop are provisional
and should be re-baselined on hornbench before being treated as authoritative.

## Baselines we measure against

| Engine | Role | Source of numbers |
|---|---|---|
| **RDFox** (Samsung / Oxford) | Materialization throughput leader. Pure forward-chaining. SPB-256 A/B driver: `scripts/run-rdfox-spb-256.sh` (requires a benchmarking license — see DeWitt note below). | ISWC 2015 paper: **6.1 M triples/sec** materialization on SPARC T5-8 (128 cores, 4 TB RAM). RDFox's own statement: pure-materialization gives up **100–1000×** on backward chaining. |
| **GraphDB Enterprise** (Graphwise) | SPARQL throughput leader. Java/RDF4J derived. | LDBC SPB published baseline: expansion ratio **1:3.2** on SPB-256 OWL 2 RL run. |
| **GraphDB Free** | Open competitor accessible without procurement. | Our differential A/B reference for nightly LDBC SPB-256 (`scripts/run-graphdb-free-spb-256.sh`). |
| **Inferray** | Transitivity-closure speed record on commodity hardware. | **21.3 M triples/sec** closure on a single Intel desktop; **142×** vs RDFox and **590×** vs GraphDB/OWLIM on transitivity-chain. |
| **Apache Jena (+ WCOJ extension)** | WCOJ reference point. | Hogan et al. ISWC '19: **1–2 orders of magnitude** speedup over baseline Jena on WatDiv shapes. |
| **DuckDB** | Per-tuple-overhead reference. | Published baseline ~**2 ns/tuple** for simpler operators. |

A note on **publication of comparative numbers**: RDFox commercial licenses typically forbid published comparative benchmarks (the so-called "DeWitt clause"). Internal use against an RDFox benchmarking license is permitted and is the Stage-1 expectation; publishing requires legal review (SPEC-01 Risks). GraphDB Free has no such restriction.

## Stage gates

These are the project-level go/no-go thresholds from `specs/SPEC-00-vision.md`.

| Stage | Workload | Target | Stop-the-line if |
|---|---|---|---|
| **Stage 1** (feasibility prototype) | LUBM-100 materialization | within **3×** of RDFox | red on selected W3C subset (≥50 cases) |
| **Stage 1** | Selected W3C OWL 2 RL subset | **100%** green | any case red |
| **Stage 2** (MVP) | LUBM-8000 materialization | within **2×** of RDFox | red on full W3C OWL 2 RL + SPARQL 1.1 + Entailment Regimes |
| **Stage 2** | LDBC SPB SF3 read | ≥**50%** of GraphDB Enterprise throughput | ORE 2015 OWL 2 RL fragment <100% solved |
| **Stage 3** (hardware specialization) — *win condition* | LDBC SPB SF5 (~1B edges) on a single MI300A or GH200 | ≥**1.5×** RDFox materialization **and** ≥**2×** GraphDB Enterprise query throughput | "Stage 3 has not earned its budget" — SPEC-09 NF5 |

> **Stage-1 LUBM gate — measurement status (internal):** wired and runnable via
> `scripts/bench/compare-rdfox.sh --lubm N`. Both engines reason over identical
> LUBM TBox+ABox with the same rule set (RDFox runs a ruleset generated from
> `crates/owlrl/rules.toml`), guarded by a closure-count parity gate and a
> wall-clock cap on HornDB. The N=1 wiring run completes end-to-end. The
> closure-count **parity** gate now passes exactly (delta 0) — the earlier
> over-derivation was a harness-completeness gap, resolved in
> [#59](https://github.com/sunstoneinstitute/horndb/issues/59).
>
> The SPEC-05 GraphBLAS closure backend is now **injectable** into the owlrl
> `Engine` (`Engine::with_backend(BackendChoice::GraphBlas)`, `graphblas-backend`
> feature) and **differential-proven equal** to the nested-loop reference
> ([#61](https://github.com/sunstoneinstitute/horndb/issues/61);
> `crates/owlrl/tests/closure_backend_differential.rs`). Per-phase profiling
> (`horndb-bench materialize --backend …`, see the *Measured* table below)
> attributes the materialize cost and shows the **timing** gate is **not** a
> closure-backend problem: on a LUBM-shaped workload (shallow class hierarchy +
> many typed instances) the closure phase is ~0.3% of reason time, dominated by
> the compiled `cax-sco` type-expansion and delta application — so swapping the
> closure backend alone does **not** clear the 3× gate. That gap is the
> SPEC-04 F5 `rdf:type`-partition-scan work tracked in
> [#133](https://github.com/sunstoneinstitute/horndb/issues/133). The GraphBLAS
> backend is a large win only when closure itself dominates (a transitive-property
> chain: ~318× on the closure phase vs nested-loop). LUBM-100 (the literal gate)
> not yet run — LUBM generation needs Jena `riot`, unavailable in the current
> sandbox; the attribution above is from synthetic stand-ins of each regime.
> RDFox comparison numbers are internal only (DeWitt clause) and are never
> recorded here.

## Per-subsystem targets (Stage 2 unless noted)

Numbers below are pulled directly from each SPEC's NF section and acceptance criteria. They are the floor each subsystem must hit before it's "done."

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
| `eq_rep_p_skew` bench — `eq-rep-p` class canonicalization (k=32 mutual-`owl:sameAs`, rows=8) | optimized path ≤ naive, identical closure (differential proptest) | this PR: **38.1 ms** optimized vs **48.7 ms** naive (~1.28×). Output blow-up is semantically irreducible; downstream F5 partition-scan now parallelised (next row) |
| `rdf_type_skew` bench — F5 `rdf:type` partition-by-class parallelism (`cls-int1` over a width-12 intersection, skewed `c1` extent) ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)) | parallel (`Auto`) ≤ serial, **identical** closure (`tests/rdf_type_skew_differential.rs`, incl. proptest) | _macOS dev workstation, 2026-06-18:_ 100 k subjects **172.6 ms** `Auto` vs **199.6 ms** `Serial` (~**1.16×**); 50 k **81.3 ms** vs **91.9 ms**. The win is over the whole `materialize` (apply + closure phases dominate, so the per-subject parallelism is diluted); the rule-local speedup is larger. Subject extents below `PAR_TYPE_THRESHOLD` (256) run sequentially. Each rule dedups its heads to O(distinct heads) before collecting, so the parallel path never allocates per-membership/per-pair duplicates. Compiled-rule (`cax-sco`-style) parallelism is a separate Stage-2 follow-up. |

### SPEC-05 — GraphBLAS closure backend (`horndb-closure`)

| Metric | Target | Baseline |
|---|---|---|
| Transitive closure (25K-node Inferray-shape chain) | ≥**10 M triples/sec** | Inferray 21.3 M (NF1; we pay for GraphBLAS generality) |
| Transitivity-chain (2,500 nodes) | ≥**10×** RDFox, ≥**50×** GraphDB/OWLIM | Inferray 142× / 590× (acceptance #1, looser to absorb integration overhead) |
| LUBM-8000 closure memory | ≤**2×** original transitive-property triples | NF3 / acceptance #5 |
| Closure vs SPEC-04 rule-firing (LUBM-100) | **identical** triple set | acceptance #4 |
| Routing heuristic | SPEC-04 if `nnz(M_p) < 10⁴`, else SPEC-05 | Risks — threshold needs bench tuning |
| Incremental single-edge insert vs full recompute (F6, 2,000-node chain) | incremental ≪ full recompute | `benches/incremental.rs` — see Measured section below. |
| Valued-reasoning readiness (#11) — valued `(max,×)` vs boolean `(∨,∧)` closure; generic-kernel penalty | _instrument, then decide_ | `benches/valued_readiness.rs` — see Measured section below. Gated custom-semiring work ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)). |
| Valued best-confidence crosswalk closure (#12 Fork A) — built-in `(max,×)` vs boolean reachability on a GTIO/SKOS-shaped crosswalk graph | _Fork-A closure is a large win over SPARQL property-path crawling_ | `benches/crosswalk.rs` — see Measured section below. |

### SPEC-06 — DBSP incremental maintenance (`horndb-incremental`)

| Metric | Target | Baseline |
|---|---|---|
| Steady-state insert/retract latency (LUBM-1000 warm) | ≤**100 ms** | NF1 / acceptance #1 (jointly owned with SPEC-04 NF3) |
| Sustained insert throughput (warm LUBM-8000) | ≥**100K triples/sec** | NF2 / acceptance #2 |
| Query-latency degradation under sustained write load | ≤**2×** no-write baseline | acceptance #2 |
| Pending delta size between checkpoints | ≤**5%** of main store | NF3 |

> **Stage 1 reality check:** NF1 and NF2 are *Stage-2 gates*. Stage-1 ships only the criterion benchmark scaffold (`benches/insert_throughput.rs`) on a synthetic 10K-triple fixture so regressions become visible as the real engine lands. Retraction is deferred entirely — see `crates/incremental/FUTURE-WORK.md`.

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
| Chain-rule closure throughput (SSSOM mappings) | **TBD** (NF1) | RDFox/Inferray closure leaders; Measured: pending hornbench (F5/F6 follow-up) |
| Compact crosswalk-index footprint | ≤**10 bytes/pair** bidi (NF2, F5) | EF+FOR baseline → rung-4 PGM; Measured: pending hornbench (F5/F6 follow-up) |
| Full-closure materialization vs OxO2 | beat **1.16 M mappings / 17 min** (NF3) | OxO2 (EBI Ontology Xref Service) reference run; Measured: pending hornbench (F5/F6 follow-up) |

### SPEC-12 — SIMD acceleration layer (`horndb-simd`)

| Metric | Target | Baseline |
|---|---|---|
| WCOJ per-tuple overhead (`per_tuple`) | ≤**2.5 ns/tuple** | DuckDB ~2 ns; closes the SPEC-03 NF1 5 ns envelope (NF1) |
| Sorted-set intersection SIMD speedup (`intersect`) | ≥**4×** AVX-512 / ≥**2×** NEON vs scalar | NF2 |
| Bulk dictionary decode SIMD speedup | ≥**4×** scalar | NF4 |
| `rdf:type` partition scan | ≥**80% STREAM Triad** bandwidth | SPEC-02 NF2 / acceptance #4 (jointly owned) |
| Per-kernel differential vs scalar oracle | **zero** mismatches, every ISA path | NF3 |

> SIMD is scoped to the loops that are already *algorithmically right* — WCOJ
> seek/intersect, dictionary decode, columnar scans. It is **not** the lever for the
> `cax-sco` / `rdf:type` materialization hotspot, which is an un-indexed full-partition
> scan fixed by an object index + semi-naïve firing
> ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)) — see SPEC-12 §F3.

## Where we actually are right now

Honest accounting. Updated when a bench moves.

### Measured

| Bench | Crate | Spec target | Measured | Verdict |
|---|---|---|---|---|
| 4-cycle, ~10⁶-edge synthetic (`benches/four_cycle.rs`) | `horndb-wcoj` | WCOJ ≥10× binary-hash | _macOS dev workstation (2026-05-31, [#1](https://github.com/sunstoneinstitute/horndb/issues/1), canonical skewed win case, 1,021,610 edges, `hub_out=32`):_ WCOJ **0.55 s** (median; [0.45, 0.68]) vs binary-hash **18.8 s** ([17.6, 20.2]) → **~34× faster**. _Reconfirmed 2026-06-17 (macOS dev workstation): WCOJ **462 ms** ([456, 470]) vs binary-hash **14.03 s** ([13.69, 14.40]) → **~30×** — same shape, sample-to-sample wobble._ _Earlier on the old uniform low-degree graph the ratio was only ~1.15× (dense) / 1.11× (compressed, [#15](https://github.com/sunstoneinstitute/horndb/issues/15))._ | **GREEN — Stage-1 acceptance #2 met** ([#1](https://github.com/sunstoneinstitute/horndb/issues/1)). The gate is a *graph-shape* problem, not bandwidth: a uniform low-degree graph never forces the intermediate-result blow-up. The canonical skewed win case (`SyntheticGraph::skewed_four_cycle`: high-out-degree hubs + a thin, dedicated closure) makes a binary join materialise the full `#2-paths · hub_out ≈ 3.2·10⁷` 3-path relation over every source, while WCOJ evaluates depth-first and never materialises an intermediate — the cycle-closing intersection is empty for almost every `(a,b,c)` prefix, so it backtracks in O(1) without expanding the hubs (a ≈`hub_out` advantage). Correctness vs an independent brute-force count (including the rotational matches a single-predicate cycle admits) is pinned by `tests/skewed_four_cycle.rs`. |
| Differential fuzzer, 1024 random BGPs (`tests/differential_fuzz.rs`) | `horndb-wcoj` | zero mismatches vs binary-hash | green at 256 cases on default seed; `#[ignore]` removed; regression file deleted. _Reconfirmed 2026-06-17: 2/2 tests pass, zero mismatches._ | **GREEN — Stage-1 acceptance #3 met** (TASKS.md CRITICAL closed) |
| `spec05_incremental_append` — single-edge append on a 2,000-node chain | `horndb-closure` | incremental ≪ full recompute | this PR (macOS dev workstation, 2026-06-01): incremental_insert **393 µs** vs full_recompute **453 ms** (~**1,153×**). _Reconfirmed 2026-06-17 (macOS): incremental_insert **753 µs** (median, wide spread [648 µs, 929 µs]) vs full_recompute **660 ms** → **~878×**._ | **GREEN** — insertion-only F6; differential-proven equal to GraphBLAS closure (`tests/incremental.rs`). |
| owlrl materialize A/B, closure-backend swap (`horndb-bench materialize --backend rulefiring\|graphblas`) | `horndb-owlrl` + `horndb-closure` | RuleFiring vs GraphBLAS — identical closure; attribute LUBM materialize cost ([#61](https://github.com/sunstoneinstitute/horndb/issues/61)) | _Linux dev server (Debian 13), release, 2026-06-08, **synthetic** stand-ins (Jena `riot` unavailable → LUBM not generated):_ **(a) LUBM-shaped** (12-class chain + 40 k typed instances, 440,117 inferred — **identical** both backends): reason **528 ms** RuleFiring vs **505 ms** GraphBLAS; phase split ≈ compiled-rules **282 ms** / apply **200 ms** / **closure 1.6 ms**. **(b) closure-heavy** (600-node transitive-property chain, 182,463 inferred — **identical** both backends): closure phase **49,649 ms** RuleFiring vs **156 ms** GraphBLAS (**~318×**), total **49.7 s → 0.25 s**. _Reconfirmed 2026-06-17 (macOS dev workstation, synthetic stand-ins via `gen_workload.py`): **(a)** taxonomy d=12/40 k inst, 480,128 inferred **identical** both backends, reason **627 ms** RuleFiring vs **612 ms** GraphBLAS, closure phase ~4 ms (within noise — gap is compiled-rules ~360 ms + apply ~200 ms). **(b)** 600-node `owl:TransitiveProperty` chain (needs `gen_workload.py chain … --transitive`), 179,163 inferred **identical** both, closure phase **80,198 ms** RuleFiring vs **155 ms** GraphBLAS (**~517×**), total **80.3 s → 0.24 s**._ | **Backend wired + parity GREEN; LUBM 3× timing gate still open and NOT closure-bound.** On LUBM-shaped work the closure phase is ~0.3% of reason time, so the GraphBLAS swap is within noise — the gap is the compiled `cax-sco`/`rdf:type`-scan + delta-apply cost ([#133](https://github.com/sunstoneinstitute/horndb/issues/133), SPEC-04 F5). GraphBLAS wins decisively only when closure dominates (regime b). Differential parity: `crates/owlrl/tests/closure_backend_differential.rs`. |
| `valued_readiness` — valued-reasoning readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11)) | `horndb-closure` | Instrument valued `(max,×)` closure to decide *when* custom-semiring/JIT work ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)) pays off — measure, don't guess | **`hornbench` (16-core Linux, OpenMP, criterion median; 2026-06-18, single-predicate `(0,0.99)`-weighted n-chain).** Re-baselined from the earlier provisional laptop run under [#12](https://github.com/sunstoneinstitute/horndb/issues/12). **(1) Valued-vs-boolean kernel split** (same shape, closure to fixpoint, incl. the exact GraphBLAS-side fixed-point check): N=500 (nnz=499→124,750 pairs) boolean `(∨,∧)` **6.76 ms** vs valued `(max,×)` builtin **37.2 ms** → valued **~5.5×** boolean; N=2,500 (nnz=2,499→3,123,750 pairs) boolean **156.8 ms** vs valued **10.86 s** → **~69×**. The valued penalty grows steeply with result `nnz` **and** with core count: hornbench's 16-core OpenMP parallelises boolean's iso/bitmap closure across cores, while the FP64 non-iso accumulation stays effectively serial — so the split is much wider here than on the single-thread-bound laptop (~3.8× at N=2,500). This is the price of carrying a scalar confidence at all; it is **not** a kernel-speed problem JIT could fix (see (2)). **(2) Generic-kernel penalty** (built-in FactoryKernel vs user-defined-op generic kernel, *same* `(max,×)` semantics): N=500 builtin **36.8 ms** vs UDF **37.8 ms** → **~1.03×**; N=2,500 builtin **10.80 s** vs UDF **10.81 s** → **~1.0×** (inside the run-to-run spread). The scalar-FP64 user op is **not** meaningfully slower than the FactoryKernel on the vendored SuiteSparse v10.3.x build, even at 16-core scale. | **GREEN — instrumentation delivered, numbers measured on `hornbench`.** **Decision rule (recorded):** _stay on built-in semirings while the carrier is scalar_ — a scalar **Fork A** confidence needs no custom op, and the valued-vs-boolean cost is a property of the carrier, not the kernel. _Custom semiring only for a **structured** carrier_ (Fork B). _PreJIT only when the generic-kernel share × generic→inlined speedup crosses the SLO_ — **measured ~1.0× generic-kernel penalty for a scalar FP64 op (even at 16-core scale), so PreJIT buys ≈0 here**; revisit only for a structured/UDT carrier where the generic kernel is the only option. Correctness pinned by `tests/valued_closure.rs` (builtin≡UDF bit-identical; weight-only-improvement + UDF-survives-semiring-drop regressions). **Fork A delivered ([#12](https://github.com/sunstoneinstitute/horndb/issues/12), `crosswalk` row below); Fork B / PreJIT deferred.** |
| `intersect` — sorted-set intersection SIMD-vs-scalar ([#132](https://github.com/sunstoneinstitute/horndb/issues/132)) | `horndb-simd` | ≥**4×** AVX-512 / ≥**2×** NEON vs scalar (SPEC-12 NF2, acceptance #3) | **(pending hornbench measurement)** — bench wired (`crates/simd/benches/intersect.rs`, scalar/avx2/avx512/neon legs via `with_forced_isa`) and smoke-run locally; the shipped per-ISA kernels are the correctness-first galloping form (differential-proven equal to the scalar oracle, `tests/differential.rs`), so the SIMD speedup is **not** yet expected to clear the floor — the wide compress/compare kernel lands only once this bench shows the galloping form misses 4×. Record AVX2/AVX-512/scalar ratios on `hornbench` (EPYC Zen4) and, if AVX-512 downclocks below AVX2, flip the production preference in `intersect::resolve()`. | **PENDING — bench wired, awaiting hornbench numbers.** |
| `crosswalk` — Fork-A best-confidence crosswalk closure ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)) | `horndb-closure` | Fork A: resolve best-confidence crosswalk/propagation mappings in one built-in `(max,×)` closure instead of a SPARQL property-path crawl; bench it on a GTIO/SKOS-shaped graph and quantify the scalar-confidence carrier cost vs boolean reachability | **`hornbench` (16-core Linux, OpenMP, criterion median; 2026-06-18).** GTIO/SKOS-shaped layered crosswalk DAG: `vocabs` source vocabularies, each a `depth`-deep `skos:broader` ladder, cross-linked into the next vocab by competing `exactMatch`(0.90)/`closeMatch`(0.60) edges the closure must arbitrate. **Carrier-cost comparison** (matrix is *prebuilt* outside the timed loop — construction/renumbering is not the workload of interest; both legs then time the same thing on the prebuilt matrix: close to fixpoint → read `nvals`, *no* tuple extraction — so only the carrier differs and the ratio is apples-to-apples): **v8_d32** (256 concepts) valued `(max,×)` **2.55 ms** vs boolean `(∨,∧)` **1.10 ms** → **~2.3×**; **v16_d64** (1,024 concepts) valued **50.9 ms** vs boolean **19.6 ms** → **~2.6×**. **End-to-end Fork-A entry point** (`CrosswalkGraph::best_confidence_closure` on a prebuilt graph — the query cost, incl. extracting + mapping every best-confidence pair back to dictionary IDs): v8_d32 **2.58 ms**, v16_d64 **49.8 ms** — i.e. the result extraction + ID remap adds ≈0 over the raw closure at these shapes. The scalar-confidence carrier is a steady ~2.3–2.6× over boolean reachability on this denser, well-parallelisable shape (a far smaller penalty than the n-chain's 16-core split, where boolean's iso/bitmap fast path runs away — here both legs do comparable real work). Both finish in single/double-digit ms, i.e. one matrix closure replaces an unbounded property-path walk. | **GREEN — Fork A delivered.** Best-confidence semantics correctness-pinned by `tests/crosswalk.rs` (2-hop chain beats weak direct edge; GTIO/SKOS resolution; sparse dictionary IDs; duplicate-edge max; unknown/identity → `None`; out-of-`(0,1]` confidence rejected). The ~2.3–2.6× carrier cost confirms the #11 decision rule: a scalar confidence stays on the built-in semiring (no kernel speed to reclaim), and **Fork B (structured carrier) / PreJIT are deferred** (SPEC-05 valued-reasoning addendum). |

### Scaffolded but not yet evaluated against targets

These benches compile and run on synthetic fixtures so future regressions are visible. They do not yet exercise the workload the SPEC measures, and the numbers they produce should not be compared to the target column above.

| Bench | Crate | Notes |
|---|---|---|
| `benches/per_tuple.rs` | `horndb-wcoj` | SPEC-03 NF1 sanity check, **now owned by SPEC-12 NF1** (target ≤2.5 ns/tuple). Stub today; the real microbench + SIMD seek/intersect land with SPEC-12 (no SIMD yet, binary-search-based seek). |
| `benches/insert_throughput.rs` | `horndb-incremental` | SPEC-06 NF1/NF2 scaffold. Synthetic 10K-triple fixture — LUBM-1000 and LUBM-8000 are Stage-2 work. |
| `benches/load_lubm.rs` | `horndb-storage` | SPEC-02 F8 / acceptance #1 scaffold. |
| `benches/transitive.rs` | `horndb-closure` | SPEC-05 NF1 / acceptance #1 scaffold. |
| `benches/sameas.rs` | `horndb-closure` | SPEC-05 `owl:sameAs` equivalence-class scaffold. |
| `benches/incremental.rs` | `horndb-closure` | SPEC-05 F6 incremental insert vs full recompute. |
| `benches/valued_readiness.rs` | `horndb-closure` | #11 valued-reasoning readiness metrics — now in the Measured table above (valued-vs-boolean split + generic-kernel penalty). |
| `benches/crosswalk.rs` | `horndb-closure` | #12 Fork-A best-confidence crosswalk closure — now in the Measured table above (valued `(max,×)` vs boolean reachability on a GTIO/SKOS-shaped graph). |
| `benches/four_cycle.rs` (binary-hash leg) | `horndb-wcoj` | Reference half of the comparison above. |

### Not yet running

- **LUBM-8000 materialization** (SPEC-04 acceptance #2, SPEC-02 acceptance #2/#3). Gated on the storage + rule engine being usable on real corpora.
- **ORE 2015 OWL 2 RL fragment full pass.** Ten-ontology subset is wired up (`harness/ore2015-selected.toml`); the full corpus expansion is Stage-2 work (TASKS.md MEDIUM).

### Running — LDBC SPB nightly (published)

- **LDBC SPB nightly — running end-to-end.** `.github/workflows/nightly.yml` brings HornDB up per-run via `crates/harness/scripts/start-engine.sh` (serving the prepared flat closure, no reasoning) and drives the SPB aggregation query mix against `/query` + `/update`, recording `aggregation-qps` into the trend DB (`harness report --suite ldbc-spb-256 --metric aggregation-qps`). Validated on `hornbench` 2026-06-25 at **feasible scale** — the 512 k-triple materialized SPB closure (BBC ontologies + reference datasets + 200 k generated Creative Works), aggregation-only. **Follow-up (TASKS.md):** the true SF=0.256 (256 M-triple) dataset and editorial (CW insert/update/delete) agents; until then `editorialAgents=0` and the headline metric is `aggregation-qps`, not `editorial-qps`.
- **A/B vs GraphDB Free — wired.** SPEC-01 F10. GraphDB **10.8.14** runs as a standing, licence-free service on `hornbench` (GraphDB 11.x hard-requires a licence even for the free tier — "No license was set"; 10.x keeps the genuine free tier), provisioned by `crates/harness/scripts/bootstrap-graphdb-free-spb.sh` and holding the same closure in a no-inference `spb` repo for an apples-to-apples query A/B. The nightly's A/B leg skips gracefully (warning, not failure) if GraphDB is unreachable. _First-light feasible-scale smoke (2026-06-25, 30 s window): HornDB **0.23** vs GraphDB Free **145.9** aggregation-qps on the 512 k-triple closure — a large gap the nightly now tracks._ No licence restriction on publishing GraphDB Free numbers. **Durability follow-up (TASKS.md):** make GraphDB a systemd unit so it survives a hornbench reboot.

### Running, internal only (no published numbers)

- **A/B vs RDFox.** SPEC-01 F10 — **implemented and runnable** via `scripts/bench/compare-rdfox.sh` (see `scripts/bench/README.md`). It times HornDB against RDFox on identical inputs for three goal-bearing operations: bulk N-Triples import (SPEC-02 F8), transitive closure (SPEC-05 acceptance #1), and OWL 2 RL materialization (the Stage-1 LUBM gate). Per the DeWitt-clause note under *Baselines*, the measured numbers are **internal only** — results are written to gitignored `scripts/bench/results/` and are never committed. Outstanding: a real-LUBM materialization workload (the literal Stage-1 gate currently stands in with a synthetic subClassOf taxonomy), and wiring the comparison into CI / the trend DB.

## Reproducing the numbers

All measured numbers above come from `cargo bench` invocations against the relevant crate. Use `--quick` for development sweeps; record both means **and** the criterion HTML reports (under `target/criterion/`) for any number quoted in TASKS.md, a commit message, or a published artefact.

```bash
# WCOJ acceptance #2 — the headline Stage-1 perf bench
cargo bench -p horndb-wcoj --bench four_cycle

# WCOJ NF1 — per-tuple overhead microbench
cargo bench -p horndb-wcoj --bench per_tuple

# WCOJ correctness — differential fuzzer (green; #[ignore] removed, regression file deleted)
cargo test -p horndb-wcoj --test differential_fuzz

# SPEC-06 incremental insert throughput
cargo bench -p horndb-incremental --bench insert_throughput

# SPEC-02 storage — LUBM load throughput
cargo bench -p horndb-storage --bench load_lubm

# SPEC-05 closure — transitive and sameAs
cargo bench -p horndb-closure --bench transitive
cargo bench -p horndb-closure --bench sameas

# SPEC-05 closure — valued-reasoning readiness metrics (#11)
cargo bench -p horndb-closure --bench valued_readiness
```

End-to-end conformance and benchmark runs go through the harness binary; see [`README.md`](README.md#run-the-conformance-harness) and [`crates/harness/README.md`](crates/harness/README.md). Results persist to `target/harness.sqlite` and are queryable via `harness report`.

## Updating this document

When a bench moves into "Measured" (or moves between RED and GREEN), update the relevant row, link the commit that closed the gap, and remove the corresponding entry from `TASKS.md`. The harness already records `(commit-sha, suite, hardware, throughput-metric, latency-metric)` per run (SPEC-01) — this file is the human-readable index into that store, not a replacement for it.
