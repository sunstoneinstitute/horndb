# SPEC-09 Hardware Specialization — Stage 3 Roadmap

> **For agentic workers:** This is a **ROADMAP**, not an executable TDD plan. It contains no per-task code; Stage 3 implementation plans are written *after* the entry gate completes. Do not execute tasks from this document directly — use it to inform the Stage-1/2 plans (the "Early Groundwork" section) and to bootstrap the real Stage-3 plans once prerequisites are met.

**Goal:** Define when and how Stage 3 (GPU / CXL / multi-node specialization) begins, what must be true before it can begin, and what trait/API hooks earlier specs should land *now* to make Stage-3 backends pluggable rather than invasive.

**Architecture:** Stage 3 plugs alternative backends into stable interfaces owned by SPEC-02 (storage tier), SPEC-03 (join executor), and SPEC-05 (closure engine). Nothing in Stage 3 invents a new architecture; it provides faster implementations of existing traits. The `horndb-hardware-ext` crate is the integration seam.

**Tech Stack (Stage 3):** CUDA 12.x / ROCm 6.x; SuiteSparse:GraphBLAS GPU branch (or cuSPARSE/hipSPARSE fallback); CXL 2.0/3.0 via `/dev/dax` mmap; NVIDIA GPUDirect Storage / BaM; DBSP timely-dataflow for multi-node.

---

## Status

**Stage:** 3 — gated on Stage 2 completion. Per SPEC-00 §Roadmap stages and SPEC-09 §Dependencies, no Stage-3 implementation work begins until **every Stage-2 acceptance criterion is green in CI**. Performance optimization on a non-conforming engine is wasted work.

**Estimated calendar window:** ~Month 16–28 of project lifecycle (12 months, +1–2 engineers on top of the Stage-2 team). Do not commit to dates until Stage 2 is within one milestone of done.

---

## Prerequisites (hard gate — all must be true before Stage 3 starts)

- [ ] **P1. Stage 2 acceptance criteria 100% green.** Full W3C OWL 2 RL test suite, full SPARQL 1.1 Test Suite, SPARQL 1.1 Entailment Regimes (OWL 2 RL/RDF), ORE 2015 OWL 2 RL fragment 100% solved, LDBC SPB SF3 ≥50% of GraphDB Enterprise throughput, LUBM-8000 materialization within 2× of RDFox — all measured by SPEC-01's harness, all green in main-branch CI for at least one full release cycle.
- [ ] **P2. Reference hardware procured and racked.** At minimum: one AMD MI300A node, one NVIDIA GH200 (or GB200) node, one CXL-attached DRAM box (Astera Labs Leo or equivalent), one NVMe Gen5 SSD with GPUDirect Storage support. Power and cooling budget signed off (MI300X ≈750 W, B200 ≈1000 W).
- [ ] **P3. Toolchain readiness audit.** CUDA 12.x and ROCm 6.x both installable in CI; SuiteSparse:GraphBLAS GPU branch tested for production readiness (decision point: use upstream GPU path vs fall back to direct cuSPARSE / hipSPARSE per SPEC-09 §Risks). Document the chosen path in an ADR before starting F1.
- [ ] **P4. Legal review for benchmark publication.** RDFox and GraphDB Enterprise license terms reviewed for clauses restricting published comparison (DeWitt-style clauses). NF5's "win condition" is internally verifiable regardless, but any external marketing / paper / blog post depends on this. Output: a written legal opinion archived in the repo.
- [ ] **P5. SPEC-02/03/05 stable trait surface.** The hooks listed in **Early Groundwork** below are merged on `main` and have been stable for ≥1 minor release. If they are still churning, Stage 3 starts blocked on a mini-stabilization phase.
- [ ] **P6. Differential-testing infrastructure in SPEC-01.** Harness can run the same conformance suite against two backends in parallel and diff results triple-by-triple. Required by F7 (every hardware backend passes the same conformance bar) and by NF4 in SPEC-05 (determinism).
- [ ] **P7. Multi-node deployment story drafted.** Not implemented — drafted. Membership, failure detection, partition rebalancing decisions made on paper (and documented as an ADR) before any timely-dataflow code lands. SPEC-09 §Risks calls this out as the operationalisation bottleneck.

---

## Stage 3 Workstreams

Each workstream below corresponds to one or more SPEC-09 functional requirements. Person-month (PM) estimates assume one experienced engineer; parallel-friendly workstreams can run concurrently with appropriate coordination.

| # | Workstream | Covers | Rough size | Ordering | Owner profile |
|---|------------|--------|------------|----------|---------------|
| WS1 | **GPU GraphBLAS closure backend** | F1, NF1 | 4–6 PM | Starts first — least integration risk, isolated behind SPEC-05 trait | GraphBLAS / CUDA expert |
| WS2 | **GPU WCOJ + subgraph-matching dispatch** | F2, F3, NF2 | 6–9 PM | Starts after WS1 (validates the GPU plumbing pattern) | GPU kernel + query-planner experience |
| WS3 | **CXL tiering policy** | F4, NF3 | 3–5 PM | Parallel with WS1/WS2 — touches SPEC-02 tier API, independent of GPU work | Systems / memory-tiering background |
| WS4 | **GPUDirect Storage / BaM cold tier** | F5 | 3–4 PM | Starts after WS1 lands the GPU device-management substrate | Linux kernel / NVMe driver experience |
| WS5 | **Multi-node DBSP via timely-dataflow** | F6, NF4 | 6–8 PM | Starts last; depends on Stage-2 SPEC-06 being mature and on the WS1–WS4 single-node story being stable | Distributed-systems engineer |
| WS6 | **Differential conformance for all backends** | F7 | 2–3 PM | Runs continuously alongside WS1–WS5; built on prerequisite P6 | SPEC-01 owner |
| WS7 | **Win-condition benchmark + publication** | NF5, acceptance #5 | 2–3 PM | Final workstream; gated on WS1–WS6 and P4 | Whoever owns benchmarking |

**Total:** ~26–38 PM of engineering, which matches SPEC-00's Stage-3 budget of 12 months × 2–3 engineers (24–36 PM).

**Critical path:** P1 → WS1 → WS2 → WS5 → WS7. WS3, WS4, WS6 run in parallel and do not gate the critical path.

---

## Stage 3 Entry Gate — First-Week Task List

Once **all** prerequisites P1–P7 are satisfied, the first calendar week of Stage 3 executes the following before any production code is written. These are gate-keeper activities, not feature work.

- [ ] **G1. Tag the Stage-2 baseline.** Cut a `stage-2-final` git tag on `main` at the commit where all Stage-2 acceptance criteria were last verified green. Every Stage-3 benchmark compares against this tag.
- [ ] **G2. Power on the procured hardware and run a smoke test.** Boot each box, install the chosen CUDA / ROCm toolchain, run `nvidia-smi` / `rocm-smi` / `cxl list` and confirm devices visible. Record firmware versions in an ADR.
- [ ] **G3. Stand up GPU CI runners.** Add self-hosted runners labelled `gpu-cuda`, `gpu-rocm`, `cxl`, `gds` to the GitHub Actions configuration. Gate them behind the existing SPEC-01 harness so a runner that fails conformance is auto-quarantined.
- [ ] **G4. Baseline-measure the Stage-2 CPU build on the new hardware.** Run the full Stage-2 acceptance benchmark suite (LUBM-8000 materialization, LDBC SPB SF3, SNOMED CT closure, etc.) on the **CPU-only** code path on each new box. **This is the baseline every Stage-3 backend must beat.** Record results in `bench/stage-2-baseline-on-stage-3-hw/`. Without this baseline, NF1/NF2/NF5 speedup numbers are meaningless.
- [ ] **G5. Re-confirm the SuiteSparse:GraphBLAS GPU path decision.** Repeat the P3 audit on the procured hardware (not just on a dev laptop). If the upstream GPU path is still research-grade, formally adopt the cuSPARSE/hipSPARSE fallback as the Stage-3 implementation target and update WS1's plan accordingly.
- [ ] **G6. Write the first real Stage-3 plan.** Using `superpowers:writing-plans`, draft `plans/YYYY-MM-DD-spec-09-ws1-gpu-graphblas.md` with executable TDD tasks. *Then* implementation begins. Do not skip this step — Stage 3's code is not exempt from the planning discipline.
- [ ] **G7. Publish the Stage-3 charter.** A one-pager listing the workstreams, owners, target dates, and the NF5 win condition. Posted somewhere the team can hold itself to it.

---

## Early Groundwork — Hooks for OTHER plans to adopt during Stage 1/2

These are **not tasks for this plan**. They are recommendations to fold into the SPEC-02, SPEC-03, SPEC-05, and SPEC-06 implementation plans *now*, so that Stage 3 can swap in alternative backends without rewriting half the engine. Each item below should appear as a referenced item in the relevant Stage-1/2 plan.

### For SPEC-02 (Storage) plans

- **Tier trait.** Express the F6 "Tier API" as a `trait TierBackend` with methods `read_triple`, `scan_predicate`, `promote`, `demote`. Even if Stage 1 ships only an HBM and a DDR5 tier, the trait shape is what lets Stage 3 add a `CxlTier` (F4) and `GpuDirectColdTier` (F5) without touching call sites.
- **Tier identity in metrics from day one.** Every read/write metric tagged with the tier it served. Required for CXL placement-policy tuning (NF3) and for diagnosing GPUDirect Storage regressions.
- **Dictionary handle abstraction.** ID-to-term lookup should go through a trait, not a concrete `Dictionary` struct, so a GPU-resident dictionary mirror (needed for F5) can be substituted without rewriting the query path.
- **NUMA-aware allocator hooks.** SPEC-02 §Risks defers NUMA placement to Stage 3, but the allocation API should accept an optional `NumaNode` hint now. Adding the parameter later is a breaking change; adding it now and ignoring it is free.

### For SPEC-03 (WCOJ) plans

- **Executor trait, not an executor struct.** The Leapfrog Triejoin implementation should sit behind a `trait JoinExecutor` so that `GpuLeapfrogExecutor` (F2) and `GpuSubgraphMatchExecutor` (F3) are alternative impls, not forks. The planner already needs cost-based plan choice (F2 in SPEC-03); extending that choice to "which executor backend" is a natural Stage-3 add.
- **Cost-model extension points.** Cardinality estimation (SPEC-03 F6) feeds the WCOJ-vs-binary-join cutover; in Stage 3 it also feeds the CPU-vs-GPU cutover and the kernel-envelope test for subgraph-matching dispatch. The cost model should accept named "backends" as a first-class concept, not be a closed function.
- **Result-materialization layer separated from join.** F3's GPU subgraph-matching kernel produces bindings in HBM; the materialization-to-host path must not dominate. Keep result materialization behind a trait so a GPU path can stream bindings without going through the CPU vectorized batch format.
- **Cancellation tokens propagate across FFI boundary.** SPEC-03 F7 mandates 100 ms cancellation; GPU kernels must honour the same token. Designing the cancellation primitive in pure-Rust Stage-2 code with FFI compatibility in mind (use `Arc<AtomicBool>` not a channel-bound primitive) avoids a Stage-3 rewrite.

### For SPEC-05 (GraphBLAS Closure) plans

- **Backend trait around `GrB_*` calls.** Wrap the CPU SuiteSparse:GraphBLAS calls behind a `trait ClosureBackend` with methods `mxm`, `eadd`, `assign`, `nnz`. The CPU impl calls `GrB_mxm` etc. directly; the Stage-3 GPU impl (F1) calls either the GPU GraphBLAS path or cuSPARSE/hipSPARSE depending on G5's decision. Differential testing (SPEC-05 acceptance #4) becomes trivially executable across both impls.
- **Determinism guarantees as test fixtures, not promises.** NF4 mandates bit-identical output. Add property-based tests in Stage 2 that run the CPU backend twice and assert identical output, then in Stage 3 swap one side for GPU — same fixture, instant differential test.
- **Dense-renumbering invalidation hook.** F7's renumbering cache must publish an invalidation event the GPU path can subscribe to (to invalidate its device-side copy). Add the event channel in Stage 2 even if nothing subscribes yet.

### For SPEC-06 (Incremental) plans

- **Worker-aware Z-set partitioning from day one.** F6 says multi-node is "a deployment-topology configuration, not a separate codebase." For this to be true, Stage-2 single-node DBSP must already think in terms of a `WorkerId` that happens to be 0 for single-node. Adding multi-worker concepts late is a rewrite; adding a one-element `Vec<WorkerId>` now is free.
- **Exchange-operator placeholder in the operator graph.** Even if Stage 2 never instantiates a real exchange, leave a `NoOpExchange` node in the operator-graph type so Stage 3's timely-dataflow `Exchange` swaps in by trait dispatch.

### For SPEC-01 (Harness) plans

- **Backend matrix in CI from day one.** The conformance harness should accept a `--backend=<name>` flag and the CI matrix should run it across whatever backends exist. In Stage 1/2 that's just `cpu`; in Stage 3 it becomes `cpu`, `gpu-cuda`, `gpu-rocm`, `cxl`, `multi-node`. Adding the flag now means F7 acceptance is mechanical, not architectural.
- **Differential mode (P6).** Harness can run two backends and diff outputs. Required by F7 and by SPEC-05 NF4. Cheaper to add in Stage 1 (when there's only one backend so the second slot is a no-op) than to retrofit.

---

## Future Work / Out of Scope (explicit non-goals)

Reiterated from SPEC-09 §Scope — these stay out of scope indefinitely and **no Stage-3 plan should reopen them** without an explicit project-level decision:

- **TPU support.** Speculative; no production OWL reasoner uses TPUs.
- **NPU offload** (Apple Neural Engine, AMD XDNA, Intel NPU). Speculative; programmable via MLIR/IREE but not currently productive for graph workloads.
- **Custom silicon.** Out forever.
- **FPGAs.** Out unless a specific paying customer commits.
- **CXL 3.0 fabric / pooled memory across nodes.** Stage 3 covers CXL 2.0/3.0 *attached* DRAM only. Pooled-memory fabrics are a Stage-4-or-later research bet.
- **Beating RDFox on pure single-node main-memory materialization throughput.** Still a non-goal per SPEC-00. Stage 3's win condition (NF5) is unified-memory hardware, not a fair fight on a small server.

---

## Open Questions to Resolve Before Stage 3 Starts

- ADR needed on MI300A vs GH200 as the **primary** target (SPEC-09 §Risks). Defer until P2 hardware is in hand and the ROCm-vs-CUDA software maturity is re-evaluated.
- ADR needed on SuiteSparse:GraphBLAS GPU path vs direct cuSPARSE/hipSPARSE (P3 + G5).
- ADR needed on multi-node operational model: who runs it, how is failure detected, how are partitions rebalanced (P7).
- Decision: does the project publish a public RDFox/GraphDB comparison, or keep NF5 internal? Driven by P4's legal review.

---

## Self-Review Notes

This plan deliberately contains no executable steps because Stage 3 is months away and the harness/storage/executor APIs that Stage 3 plugs into do not exist yet in stable form. The plan's value is in: (a) preventing premature Stage-3 work, (b) seeding the right trait abstractions into SPEC-02/03/05 plans *now* so Stage 3 is additive, (c) defining the entry gate so Stage 3 cannot start without baselines that make speedup claims meaningful.

When Stage 2 nears completion, this document should be revisited and the workstreams (WS1–WS7) each get their own executable TDD plan via `superpowers:writing-plans`.
