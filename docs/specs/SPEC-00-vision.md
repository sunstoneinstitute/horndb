# SPEC-00 — Project Vision & Architecture

## Purpose

Define the project's reason for existence, its primary user, the differentiating bet, and the architectural decomposition that all subsequent specs build on. This document is the canonical reference for "what we are building and why."

## What we are building

A hybrid forward/backward-chaining RDF reasoner targeting **OWL 2 RL semantics** with **SPARQL 1.1** querying. The system is designed for modern unified-memory hardware (HBM-equipped GPUs/APUs, CXL-attached DRAM tiers) and treats incremental maintenance, provenance, and correctability as first-class concerns — not afterthoughts.

The system is called **HornDB** (internally codenamed `reasoner` during early development). It is intended to be permissively open-source and EU-developed.

## Primary user

Sunstone Institute and its production data workloads (PROV-O backed graphs, mixed ABox/TBox, ~60% backward-chaining / ~40% forward-chaining workload, multi-billion-triple scale). Secondary users: any organisation needing OWL 2 RL reasoning at scale without lock-in to RDFox (Samsung) or GraphDB (Graphwise).

## Differentiating bets (the reason this project exists)

1. **Hybrid execution over pure materialization.** Materialize the schema/transitive-closure subset (subClassOf, subPropertyOf, sameAs, transitive properties); backward-chain the rest with magic sets. RDFox's pure-materialization model gives up 100–1000× on backward chaining (their own published statement); a mixed workload needs both.
2. **Unified-memory hardware as first-class target.** HBM (≈5 TB/s) for the hot working set, DDR5 for warm, CXL/NVMe for cold. RDFox's main-memory shared-everything design wastes HBM and CXL bandwidth.
3. **DBSP-style incremental maintenance.** Z-set differences instead of DRed/FBF counting. Better composition with point queries; ships in production at Materialize Inc.
4. **GraphBLAS for the closure subset.** Schema-level transitive closure as semiring matrix multiply on SuiteSparse:GraphBLAS — no production OWL reasoner does this today.
5. **Soufflé-style ahead-of-time rule compilation.** OWL 2 RL rules compiled to native Rust, not interpreted. No rule interpreter in the hot path.
6. **Provenance / correctability as a hard requirement.** Every inferred triple must trace back to its premises (proof tree). This rules out replacing symbolic reasoning with embeddings.

## Non-goals (explicit)

- **Beating RDFox on pure single-node main-memory materialization throughput.** Not a winnable fight on a small-team budget.
- **OWL 2 DL completeness.** OWL 2 RL only. DL reasoning (Konclude, HermiT, Pellet) is a separate problem class.
- **A rule-interpretation engine.** All rule sets are compiled.
- **Embedding-based "neural" reasoning as the source of truth.** ML is a force multiplier (query planning, owl:sameAs candidate generation, LLM→SPARQL translation), never the reasoner.
- **A graph database.** Not a property-graph competitor to Neo4j. RDF + SPARQL only.

## Subsystem decomposition

The architecture decomposes into the following subsystems, each with its own SPEC:

| SPEC | Subsystem | Notes |
|------|-----------|-------|
| SPEC-01 | Conformance & benchmarking harness | W3C tests, ORE 2015, LDBC SPB, LUBM. **Built first; gates every subsequent spec.** |
| SPEC-02 | Storage & dictionary encoding | Predicate-partitioned, columnar, tiered (HBM/DDR5/CXL/NVMe) |
| SPEC-03 | WCOJ query engine | Leapfrog Triejoin, vectorized, magic-sets backward chaining |
| SPEC-04 | OWL 2 RL rule engine | Soufflé-style ahead-of-time codegen, semi-naïve evaluation |
| SPEC-05 | GraphBLAS closure backend | Schema-level closure via SuiteSparse:GraphBLAS |
| SPEC-06 | DBSP incremental maintenance | Z-set differences instead of DRed/counting |
| SPEC-07 | SPARQL 1.1 frontend | Parser, planner, entailment regimes |
| SPEC-08 | ML/LLM integration boundary | Symbolic source of truth, ML as optimizer |
| SPEC-09 | Hardware specialization | GPU backend, CXL tiering, multi-node (Stage 3) |

Layering: **SPEC-01 (harness) comes first** — the test bench exists before the engine it tests, even if only a small subset of cases is selected to run on day one. SPEC-02 (storage) underlies the rest; SPEC-03 (WCOJ) is the join substrate; SPEC-04 (rules) and SPEC-05 (closure) sit on top; SPEC-06 (incremental) is orthogonal; SPEC-07 (SPARQL) is the public surface.

The harness is intentionally first to enforce a discipline: **every implementation commit lands in a repo that already knows how to grade it.** Whatever conformance subset we agree to run at a given stage must be 100% green before any feature is called done.

## Roadmap stages

**Stage 0 — Harness bootstrap (2–4 weeks, 1 engineer).** SPEC-01 minimal slice: a runner that can load *one* W3C OWL 2 test case and *one* SPARQL 1.1 test case and report pass/fail. CI integration that blocks PRs on selected-suite failures. No engine yet — just the bench. Exit criterion: a deliberately-failing reference implementation is correctly flagged red.

**Stage 1 — Feasibility prototype (3 months, 1–2 engineers).** SPEC-02 minimal slice + SPEC-03 + SPEC-04 minimal slice. Validate against LUBM-100 and a hand-picked subset of W3C OWL 2 RL test cases (target: ≥50 cases covering the most-used rules). Benchmark vs RDFox/GraphDB on LDBC SPB-256. Go/no-go: within 3× of RDFox on materialization throughput **and** 100% green on the selected W3C subset.

**Stage 2 — MVP (12 months, 3–4 engineers).** Full SPEC-02..07. Conformance subset expands to: full W3C OWL 2 RL test cases + full SPARQL 1.1 Test Suite + SPARQL 1.1 Entailment Regimes (OWL 2 RL/RDF) — all passing. ORE 2015 OWL 2 RL fragment: 100% solved. LDBC SPB SF3 throughput ≥50% of GraphDB Enterprise. LUBM-8000 materialization within 2× of RDFox.

**Stage 3 — Hardware specialization (12 months, +1–2 engineers).** SPEC-09. GPU backend for GraphBLAS closure and WCOJ. CXL tiering policy. Multi-node via DBSP timely-dataflow primitives. Conformance bar does not drop: every hardware backend passes the full Stage 2 subset.

## Implementation language and dependencies

- **Rust** for the engine (memory safety, zero-cost abstractions, mature Apache Arrow interop, Nemo precedent).
- **Apache Arrow** as the columnar in-memory exchange format.
- **SuiteSparse:GraphBLAS** via C ABI for the closure backend.
- **C++/CUDA/ROCm** kernels permitted in SPEC-09 only.

## Process rule (from the harness-first ordering)

Every SPEC's "acceptance criteria" section references a concrete subset of SPEC-01's test corpus. A SPEC is not "satisfied" until its referenced subset is green in CI. New implementation work on a subsystem may *grow* that subset; it may not bypass it.

## Success criteria (project-level)

The project succeeds if all three hold:

1. **Conformance.** 100% pass on W3C OWL 2 RL conformance and SPARQL 1.1 entailment regime tests. Non-negotiable.
2. **Competitive performance on a realistic workload.** On LDBC SPB SF5 (~1B edges), within 2× of GraphDB Enterprise read throughput and within 3× of RDFox materialization throughput, on a single MI300A or GH200 node.
3. **Provenance preserved.** Every inferred triple is justified by a derivable proof tree referencing source triples and rule instances.

## Out-of-scope success criteria

We do **not** measure success by: SPARQL 1.1 Federation support, full OWL 2 DL completeness, GeoSPARQL, or property-graph compatibility. These are deferred indefinitely.

**RDF 1.2 (triple terms) is a Stage-2 priority**, not a Stage-1 deliverable. We deliberately track the W3C RDF 1.2 standard rather than the community-driven RDF-star extension it superseded; the underlying graph model is essentially the same but the semantics and SPARQL surface are cleaner under 1.2. Stage-1 storage and SPARQL paths use catch-all arms that surface RDF 1.2 triple terms as `unreachable!` because the Stage-1 N-Triples / SPARQL 1.1 loaders cannot produce them; lifting that to real support is the Stage-2 migration tracked in `TASKS.md`.

## Open questions for SPEC-00

- License choice: Apache 2.0 vs MPL 2.0 vs AGPL. Defer to legal review; default assumption Apache 2.0.
- Project hosting: GitHub vs Codeberg vs self-hosted Gitea for EU sovereignty optics. Defer.
- Whether a separate "PROV-O extension" SPEC is needed, or whether provenance is folded into SPEC-04 (rules) and SPEC-07 (SPARQL). Default: fold in.
