# Benchmarks

Where we are, where we need to be, and what we measure against. This document is the single source of truth for the project's quantitative goals. Targets come from the per-subsystem SPECs (non-functional requirements and acceptance criteria); baselines come from the cited literature and vendor publications.

> **Status — 2026-05-25.** Stage 1 is in flight. Two performance numbers have been measured against the canonical Stage-1 benches; the rest of this document is targets and baselines that the harness (SPEC-01) will fill in as the engine grows. Live gaps are tracked in [`TASKS.md`](TASKS.md).

## Reference hardware

The "reference workstation" referenced throughout the SPECs:

- **CPU:** single AMD EPYC 9354 (Zen 4, 32C/64T)
- **DRAM:** 12-channel DDR5-4800
- **Storage:** local NVMe (HDT cold tier; SPEC-02)
- **Stage 3 only — accelerators:** AMD MI300A (preferred for unified HBM + Zen4) or NVIDIA GH200 / GB200

The harness captures a hardware fingerprint per run; comparisons are valid only within identical fingerprints (SPEC-01 NF — "we normalise by capturing the fingerprint, not by trying to normalise across hardware").

## Baselines we measure against

| Engine | Role | Source of numbers |
|---|---|---|
| **RDFox** (Samsung / Oxford) | Materialization throughput leader. Pure forward-chaining. | ISWC 2015 paper: **6.1 M triples/sec** materialization on SPARC T5-8 (128 cores, 4 TB RAM). RDFox's own statement: pure-materialization gives up **100–1000×** on backward chaining. |
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

## Per-subsystem targets (Stage 2 unless noted)

Numbers below are pulled directly from each SPEC's NF section and acceptance criteria. They are the floor each subsystem must hit before it's "done."

### SPEC-02 — Storage (`horndb-storage`)

| Metric | Target | Baseline |
|---|---|---|
| Bulk N-Triples import | ≥**1 M triples/sec** | RDFox (F8) |
| LUBM-100 bulk-import (~13 M triples) | ≤**30 s** on reference workstation | acceptance #1 |
| LUBM-8000 bulk-import (~1.1B triples) | ≤**30 minutes** on reference workstation | acceptance #2 |
| Warm-tier memory footprint | ≤**50 bytes/triple** | RDFox: 36.9 (NF1; we accept ~35% headroom for all 6 orderings) |
| Cold-tier (HDT) footprint | ≤**6 bytes/triple** amortised | NF1 |
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

### SPEC-05 — GraphBLAS closure backend (`horndb-closure`)

| Metric | Target | Baseline |
|---|---|---|
| Transitive closure (25K-node Inferray-shape chain) | ≥**10 M triples/sec** | Inferray 21.3 M (NF1; we pay for GraphBLAS generality) |
| Transitivity-chain (2,500 nodes) | ≥**10×** RDFox, ≥**50×** GraphDB/OWLIM | Inferray 142× / 590× (acceptance #1, looser to absorb integration overhead) |
| LUBM-8000 closure memory | ≤**2×** original transitive-property triples | NF3 / acceptance #5 |
| Closure vs SPEC-04 rule-firing (LUBM-100) | **identical** triple set | acceptance #4 |
| Routing heuristic | SPEC-04 if `nnz(M_p) < 10⁴`, else SPEC-05 | Risks — threshold needs bench tuning |

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

## Where we actually are right now

Honest accounting. Updated when a bench moves.

### Measured

| Bench | Crate | Spec target | Measured | Verdict |
|---|---|---|---|---|
| 4-cycle, 10⁶-edge synthetic (`benches/four_cycle.rs`) | `horndb-wcoj` | WCOJ ≥10× binary-hash | WCOJ **3.55 s** vs binary-hash **4.07 s** → WCOJ is **1.15× faster** (2026-05-25, post-perf-pass) | **RED — Stage-1 acceptance #2 not met** (TASKS.md HIGH; the 1.6× regression is gone, but the ≥10× gate still needs storage-side work — see TASKS.md note) |
| Differential fuzzer, 1024 random BGPs (`tests/differential_fuzz.rs`) | `horndb-wcoj` | zero mismatches vs binary-hash | green at 256 cases on default seed; `#[ignore]` removed; regression file deleted | **GREEN — Stage-1 acceptance #3 met** (TASKS.md CRITICAL closed) |

### Scaffolded but not yet evaluated against targets

These benches compile and run on synthetic fixtures so future regressions are visible. They do not yet exercise the workload the SPEC measures, and the numbers they produce should not be compared to the target column above.

| Bench | Crate | Notes |
|---|---|---|
| `benches/per_tuple.rs` | `horndb-wcoj` | SPEC-03 NF1 sanity check (5 ns/tuple). Stage 1 *allowed* to miss the target (no SIMD yet, binary-search-based seek). |
| `benches/insert_throughput.rs` | `horndb-incremental` | SPEC-06 NF1/NF2 scaffold. Synthetic 10K-triple fixture — LUBM-1000 and LUBM-8000 are Stage-2 work. |
| `benches/load_lubm.rs` | `horndb-storage` | SPEC-02 F8 / acceptance #1 scaffold. |
| `benches/transitive.rs` | `horndb-closure` | SPEC-05 NF1 / acceptance #1 scaffold. |
| `benches/sameas.rs` | `horndb-closure` | SPEC-05 `owl:sameAs` equivalence-class scaffold. |
| `benches/four_cycle.rs` (binary-hash leg) | `horndb-wcoj` | Reference half of the comparison above. |

### Not yet running

- **LDBC SPB-256 nightly.** Workflow exists (`.github/workflows/nightly.yml`) and points at a self-hosted runner with the SPB driver pre-installed. Requires `scripts/dev/start-engine.sh` to exist, which it does not yet (SPEC-04 territory).
- **LUBM-8000 materialization** (SPEC-04 acceptance #2, SPEC-02 acceptance #2/#3). Gated on the storage + rule engine being usable on real corpora.
- **ORE 2015 OWL 2 RL fragment full pass.** Ten-ontology subset is wired up (`harness/ore2015-selected.toml`); the full corpus expansion is Stage-2 work (TASKS.md MEDIUM).
- **A/B vs RDFox / GraphDB Free.** SPEC-01 F10 — harness flow exists; needs the competitor binaries on the benchmark runner and (for any *published* number) legal review of the RDFox license.

## Reproducing the numbers

All measured numbers above come from `cargo bench` invocations against the relevant crate. Use `--quick` for development sweeps; record both means **and** the criterion HTML reports (under `target/criterion/`) for any number quoted in TASKS.md, a commit message, or a published artefact.

```bash
# WCOJ acceptance #2 — the headline Stage-1 perf bench
cargo bench -p horndb-wcoj --bench four_cycle

# WCOJ NF1 — per-tuple overhead microbench
cargo bench -p horndb-wcoj --bench per_tuple

# WCOJ correctness — differential fuzzer (currently red, hence #[ignore])
cargo test -p horndb-wcoj -- --ignored differential_fuzz

# SPEC-06 incremental insert throughput
cargo bench -p horndb-incremental --bench insert_throughput

# SPEC-02 storage — LUBM load throughput
cargo bench -p horndb-storage --bench load_lubm

# SPEC-05 closure — transitive and sameAs
cargo bench -p horndb-closure --bench transitive
cargo bench -p horndb-closure --bench sameas
```

End-to-end conformance and benchmark runs go through the harness binary; see [`README.md`](README.md#run-the-conformance-harness) and [`crates/harness/README.md`](crates/harness/README.md). Results persist to `target/harness.sqlite` and are queryable via `harness report`.

## Updating this document

When a bench moves into "Measured" (or moves between RED and GREEN), update the relevant row, link the commit that closed the gap, and remove the corresponding entry from `TASKS.md`. The harness already records `(commit-sha, suite, hardware, throughput-metric, latency-metric)` per run (SPEC-01) — this file is the human-readable index into that store, not a replacement for it.
