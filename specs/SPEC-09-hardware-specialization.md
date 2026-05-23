# SPEC-09 — Hardware Specialization Roadmap

## Purpose

Define the Stage 3 hardware-specialization targets — GPU/APU backends, CXL-attached DRAM tiering, NVMe cold tier via GPUDirect Storage, and (later) multi-node distributed mode. These are the moves that justify the strategic claim that we can beat RDFox on $/triple-materialized despite a much smaller team.

This spec is a roadmap, not an implementation contract. Stage 1 and Stage 2 must not depend on it. Stage 3 begins after Stage 2 acceptance criteria pass.

## Scope

In scope (Stage 3):
- GPU/APU backends for SPEC-05 (GraphBLAS closure) and SPEC-03 (WCOJ join kernels).
- CXL 2.0/3.0 attached DRAM as a warm-tier extension under SPEC-02.
- NVMe cold tier via GPUDirect Storage / NVIDIA BaM for direct device-side I/O.
- Single-node hardware targets: AMD MI300A (preferred for unified HBM + Zen4), NVIDIA GH200 / GB200 (preferred for NVLink-C2C coherence and HBM3e).
- Distributed mode (later in Stage 3): multi-node via DBSP timely-dataflow primitives (SPEC-06 substrate generalises naturally).

Out of scope (indefinitely):
- TPU support. Speculative; no production OWL reasoner uses TPUs.
- NPU offload (Apple Neural Engine, AMD XDNA, Intel NPU). Speculative; programmable via MLIR/IREE but not currently productive for graph workloads.
- Custom silicon. Out forever.
- FPGAs. Out unless a specific customer commits.

## Functional requirements

**F1. GPU GraphBLAS backend.** SPEC-05's closure operators implemented via SuiteSparse:GraphBLAS GPU path (Davis et al., experimental as of research date) or directly via cuSPARSE / hipSPARSE matrix-matrix multiply on `(∨, ∧)` Boolean semiring. The CPU GraphBLAS backend remains the reference; GPU is a faster alternative that must produce identical output (differential-tested).

**F2. GPU WCOJ kernels.** SPEC-03's Leapfrog Triejoin reimplemented as a GPU kernel along the lines of cuMatch (SIGMOD'25), with trie-based intermediate storage in HBM. Used for star/cycle patterns over hot predicates that fit in HBM. Falls back to CPU WCOJ for cold patterns.

**F3. GPU subgraph matching adapter.** For BGPs that match the shape supported by STMatch / cuTS / EGSM kernels (subgraph isomorphism over labeled graphs), allow the planner to dispatch to a GPU subgraph-matching kernel as an alternative to WCOJ. The kernel produces variable bindings in HBM; the result-materialization path must avoid host-device transfer dominating.

**F4. CXL tiering policy.** SPEC-02's tier API is extended with a CXL tier sitting between DDR5 (warm) and NVMe (cold). Placement policy uses hot-page identification (HybridTier / FreqTier style, ASPLOS'25). CXL latency is ~100–200 ns higher than local DDR5; placement must account.

**F5. GPUDirect Storage / BaM cold tier.** NVMe-resident HDT-encoded blocks can be DMA'd directly to GPU HBM without staging through CPU DRAM. Used for the cold-tier scan path when the GPU executor is active.

**F6. Multi-node DBSP.** SPEC-06's Z-set delta stream extends naturally to multiple workers via timely-dataflow exchange operators. Each worker owns a partition of the predicate space; deltas exchange across workers as needed. Multi-node deployment becomes a deployment-topology configuration, not a separate codebase.

**F7. Differential tier correctness.** Every GPU / CXL / NVMe path must pass the same SPEC-01 conformance tier as the CPU/DDR5 reference path. No reduced-correctness modes for hardware compatibility.

## Non-functional requirements

**NF1. GPU closure throughput.** SuiteSparse:GraphBLAS GPU path on MI300X or H200: transitive closure on a 100M-edge graph at ≥10× the CPU GraphBLAS path on the reference workstation, accounting for transfer overhead amortised over the full closure.

**NF2. GPU WCOJ throughput.** On hot-predicate WCOJ patterns that fit in HBM, ≥5× the CPU WCOJ baseline on the reference workstation. (STMatch reports up to 3385× over cuTS in kernel terms; integration overhead dilutes this dramatically — 5× end-to-end is the realistic ask.)

**NF3. CXL tier latency.** Reads from CXL-tiered triples: p99 ≤500 ns on Astera Labs Leo or equivalent. Tier-promotion latency (CXL → DDR5) ≤10 ms for a 1 MB page.

**NF4. Multi-node scale.** 4-node deployment of the LUBM-8000 dataset achieves ≥3× the materialization throughput of a single node (≥75% scaling efficiency). 8-node: ≥5× (≥60%). Beyond 8 nodes is research territory.

**NF5. End-to-end win condition.** On LDBC SPB SF5 (~1B edges) on a single MI300A or GH200 node: outperform RDFox materialization throughput by ≥1.5× and outperform GraphDB Enterprise query throughput by ≥2×. This is the bet — if we cannot deliver here, Stage 3 has not earned its budget.

## Dependencies

- All earlier SPECs in their Stage 2 form. SPEC-09 cannot start until Stage 2 conformance passes — performance work on a non-conforming engine is wasted.
- External: CUDA / ROCm toolchains; SuiteSparse:GraphBLAS GPU branch; CXL-capable hardware; NVMe Gen5 SSDs with GDS support.
- Reference hardware procurement: at least one each of MI300A, GH200, and a CXL-attached DRAM box.

## Acceptance criteria

1. GPU GraphBLAS closure on a 100M-edge synthetic graph: ≥10× the CPU baseline on the same workstation (with GPU added).
2. GPU WCOJ on a representative SPB query workload: ≥5× CPU WCOJ for queries that fit the kernel envelope; planner correctly chooses CPU fallback for queries that do not.
3. CXL tier integrated; LUBM-8000 with 50% of triples in CXL tier: end-to-end materialization within 1.3× of fully-DDR5 baseline.
4. Multi-node mode: 4 nodes on LUBM-8000 achieve ≥3× single-node materialization throughput.
5. **Win condition**: LDBC SPB SF5 on single MI300A or GH200: ≥1.5× RDFox materialization, ≥2× GraphDB Enterprise query throughput, on identical hardware fingerprint.
6. SPEC-01 conformance suite passes against every hardware backend (CPU, CPU+CXL, GPU+CPU, multi-node).

## Risks and open questions

- **Host-device transfer.** Kernel-level papers (STMatch et al.) often understate H2D/D2H transfer cost. The integration into a full SPARQL engine has well-documented overhead; our 5× WCOJ target accounts for this but may still be optimistic.
- **SuiteSparse:GraphBLAS GPU path.** Davis et al. demonstrated GPU GraphBLAS in 2023 but the production-quality release is still maturing. Risk of being early. If the upstream GPU path is not production-ready by Stage 3 start, fall back to direct cuSPARSE / hipSPARSE for the closure semirings.
- **MI300A vs GH200 trade-off.** MI300A's APU model (unified HBM, no PCIe transfer) is conceptually better for graph workloads; GH200's NVLink-C2C achieves similar with separate dies. Decision depends on ROCm vs CUDA software maturity at procurement time.
- **CXL maturity.** CXL 2.0 hardware shipping; CXL 3.0 still nascent. Hot-page tracking policies for graph workloads are research, not product. Risk of choosing the wrong tiering heuristic and having to redo.
- **Distributed-mode complexity.** Multi-node DBSP via timely-dataflow is technically clean but operationally complex (membership, failure detection, partition rebalancing). The first real customer for multi-node mode pays the operationalisation cost.
- **NVMe / BaM cold tier.** BaM (Big Accelerator Memory) is research-grade; production NVMe-to-GPU DMA via GPUDirect Storage is shipping but driver-fragile. Worth piloting; not worth blocking on.
- **Power and physical footprint.** B200 at 1000W, MI300X at 750W: deployment now requires data-centre-class power and cooling. Cost-per-triple math must include capex and opex, not just transistor cost.
- **Benchmark licensing for publication.** Same as SPEC-01 — RDFox license terms may forbid published comparison; our win-condition NF5 is internally verifiable, but external marketing requires legal review.
