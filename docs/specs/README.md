# HornDB Specs

This directory contains the architectural specifications for **HornDB** — a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL semantics with SPARQL 1.1 querying, designed for modern unified-memory hardware.

Specs are derived from `../../initial-research.md` (the feasibility study and competitive landscape analysis). Read SPEC-00 first; the others assume it.

## Index

The **Plan** column links to each spec's Stage-1 implementation plan under `../plans/`. SPEC-00 is a vision document with no separate plan, and SPEC-09 is roadmap-only (Stage 3, gated on Stage 2 green).

| SPEC | Subsystem | Status | Plan |
|------|-----------|--------|------|
| [SPEC-00](SPEC-00-vision.md) | Project vision & architecture | Draft | — (vision) |
| [SPEC-01](SPEC-01-conformance-benchmarks.md) | Conformance & benchmarking harness — **built first** | Draft | [SPEC-01 plan](../plans/2026-05-24-SPEC-01-conformance-harness.md) |
| [SPEC-02](SPEC-02-storage.md) | Storage & dictionary encoding | Draft | [SPEC-02 plan](../plans/2026-05-24-SPEC-02-storage.md) |
| [SPEC-03](SPEC-03-query-engine.md) | WCOJ query engine | Draft | [SPEC-03 plan](../plans/2026-05-24-SPEC-03-wcoj-query-engine.md) |
| [SPEC-04](SPEC-04-rule-engine.md) | OWL 2 RL rule engine | Draft | [SPEC-04 plan](../plans/2026-05-24-SPEC-04-owl-rl-rule-engine.md) |
| [SPEC-05](SPEC-05-closure-backend.md) | GraphBLAS closure backend | Draft | [SPEC-05 plan](../plans/2026-05-24-SPEC-05-graphblas-closure-backend.md) |
| [SPEC-06](SPEC-06-incremental-maintenance.md) | DBSP incremental maintenance | Draft | [SPEC-06 plan](../plans/2026-05-24-SPEC-06-dbsp-incremental-maintenance.md) |
| [SPEC-07](SPEC-07-sparql-frontend.md) | SPARQL 1.1 frontend | Draft | [SPEC-07 plan](../plans/2026-05-24-SPEC-07-sparql-frontend.md) |
| [SPEC-08](SPEC-08-ml-integration.md) | ML / LLM integration boundary | Draft | [SPEC-08 plan](../plans/2026-05-24-SPEC-08-ml-integration.md) |
| [SPEC-09](SPEC-09-hardware-specialization.md) | Hardware specialization (Stage 3) | Roadmap | [SPEC-09 plan](../plans/2026-05-24-SPEC-09-hardware-specialization.md) (roadmap-only) |
| [SPEC-10](SPEC-10-rdflib-compatible-python-api.md) | rdflib-compatible Python API | Draft | — |
| [SPEC-11](SPEC-11-mappings.md) | SSSOM mappings & crosswalk index | Draft | — |
| [SPEC-12](SPEC-12-simd.md) | SIMD acceleration layer | Draft | — |

## Dated design specs

Numbered `SPEC-NN` files are the standing subsystem contracts above. Point design specs — narrower decisions that refine a subsystem rather than define it — use a `YYYY-MM-DD-<slug>.md` prefix and live alongside them here:

| Design spec | Refines | Status |
|------|---------|--------|
| [Shared, lock-guarded GraphBLAS build across worktrees](2026-05-31-shared-graphblas-build-design.md) | SPEC-05 (closure backend) | Approved |
| [`owlrl` `rdf:type` object index + genuine semi-naïve firing](2026-06-27-owlrl-type-index-seminaive.md) | SPEC-04 (rule engine, F5-adjacent; [#2](https://github.com/sunstoneinstitute/horndb/issues/2)) | Draft |

## Reading order

- **Skim SPEC-00 first** — it names the bets, the non-goals, and the stage gating.
- **Then SPEC-01** — the harness is the bench every other spec is graded against. Built before the engine; selected subset must always be green.
- For implementers: SPEC-02 → SPEC-03 → SPEC-04 → SPEC-05 → SPEC-06 → SPEC-07 follows the dependency order.
- SPEC-08 (ML) and SPEC-09 (hardware) are optional and Stage 2/3 respectively.
- SPEC-10 (Python / rdflib compatibility) sits on top of SPEC-07 and is useful once you want a Python-facing surface.

## Spec format

Each spec follows roughly this structure:
- **Purpose** — one paragraph on why this spec exists.
- **Scope** — what is in and what is out.
- **Functional requirements** (F*) — what the subsystem must do.
- **Non-functional requirements** (NF*) — performance, conformance, latency targets.
- **Dependencies** — other specs and external libraries.
- **Acceptance criteria** — measurable checks for "this spec is satisfied."
- **Risks and open questions** — known unknowns; deferred decisions.

These are living documents. When acceptance criteria are met, update the status in this table. When trade-offs change, update the spec and record the decision in the project's [ADR log](../adr/README.md).
