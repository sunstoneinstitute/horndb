# HornDB Specs

This directory contains the architectural specifications for **HornDB** — a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL semantics with SPARQL 1.1 querying, designed for modern unified-memory hardware.

Specs are derived from `../../initial-research.md` (the feasibility study and competitive landscape analysis). Read SPEC-00 first; the others assume it.

## Index

The **Plan** column links to each spec's Stage-1 implementation plan under `../plans/`. SPEC-00 is a vision document with no separate plan, and SPEC-09 is roadmap-only (Stage 3, gated on Stage 2 green).

| SPEC | Subsystem | Status | Plan |
|------|-----------|--------|------|
| [SPEC-00](SPEC-00-vision.md) | Project vision & architecture | Draft | — (vision) |
| [SPEC-01](SPEC-01-conformance-benchmarks.md) | Conformance & benchmarking harness — **built first** | Draft | [SPEC-01 plan](../plans/PLAN-01-01-conformance-harness.md) |
| [SPEC-02](SPEC-02-storage.md) | Storage & dictionary encoding | Draft | [SPEC-02 plan](../plans/PLAN-02-01-storage.md) |
| [SPEC-03](SPEC-03-query-engine.md) | WCOJ query engine | Draft | [SPEC-03 plan](../plans/PLAN-03-01-wcoj-query-engine.md) |
| [SPEC-04](SPEC-04-rule-engine.md) | OWL 2 RL rule engine | Draft | [SPEC-04 plan](../plans/PLAN-04-01-owl-rl-rule-engine.md) |
| [SPEC-05](SPEC-05-closure-backend.md) | GraphBLAS closure backend | Draft | [SPEC-05 plan](../plans/PLAN-05-01-graphblas-closure-backend.md) |
| [SPEC-06](SPEC-06-incremental-maintenance.md) | DBSP incremental maintenance | Draft | [SPEC-06 plan](../plans/PLAN-06-01-dbsp-incremental-maintenance.md) |
| [SPEC-07](SPEC-07-sparql-frontend.md) | SPARQL 1.1 frontend | Draft | [SPEC-07 plan](../plans/PLAN-07-01-sparql-frontend.md) |
| [SPEC-08](SPEC-08-ml-integration.md) | ML / LLM integration boundary | Draft | [SPEC-08 plan](../plans/PLAN-08-01-ml-integration.md) |
| [SPEC-09](SPEC-09-hardware-specialization.md) | Hardware specialization (Stage 3) | Roadmap | [SPEC-09 plan](../plans/PLAN-09-01-hardware-specialization.md) (roadmap-only) |
| [SPEC-10](SPEC-10-rdflib-compatible-python-api.md) | rdflib-compatible Python API | Draft | — |
| [SPEC-11](SPEC-11-mappings.md) | SSSOM mappings & crosswalk index | Draft | — |
| [SPEC-12](SPEC-12-simd.md) | SIMD acceleration layer | Draft | — |

## Point / design specs (SPEC-13+)

SPEC-00..12 above are the standing subsystem contracts. Point/design specs — narrower decisions that refine a subsystem rather than define it — use the same `SPEC-NN-<slug>.md` naming (next free number) and live alongside them here. Each file's frontmatter carries `status:` / `date:` / `scope:` (see `AGENTS.md`).

| SPEC | Design spec | Refines | Status |
|------|-------------|---------|--------|
| [SPEC-13](SPEC-13-shared-graphblas-build.md) | Shared, lock-guarded GraphBLAS build across worktrees | SPEC-05 (closure backend) | Approved |
| [SPEC-14](SPEC-14-lubm-rdfox-comparison.md) | Real LUBM-100 materialization comparison vs RDFox | SPEC-01 (benchmarks) | Approved |
| [SPEC-15](SPEC-15-owlrl-type-index-seminaive.md) | `owlrl` `rdf:type` object index + genuine semi-naïve firing | SPEC-04 (rule engine, F5-adjacent; [#133](https://github.com/sunstoneinstitute/horndb/issues/133)) | Draft |
| [SPEC-16](SPEC-16-id-based-slot-rows.md) | id-based slot rows for the SPARQL runtime | SPEC-07 ([#128](https://github.com/sunstoneinstitute/horndb/issues/128)) | Implemented |
| [SPEC-17](SPEC-17-metrics.md) | Metrics & observability (Phase 1: metrics) | cross-cutting (`crates/metrics/`) | Specified |
| [SPEC-18](SPEC-18-spb-driver-report.md) | SPB-256: record the full driver report | SPEC-01 (harness trend DB) | Approved |
| [SPEC-19](SPEC-19-streaming-runtime-pushdown.md) | Streaming SPARQL runtime + projection/aggregate pushdown | SPEC-07 ([#143](https://github.com/sunstoneinstitute/horndb/issues/143), [#144](https://github.com/sunstoneinstitute/horndb/issues/144)) | Approved |
| [SPEC-20](SPEC-20-join-probe-streaming.md) | Probe-side streaming Join/LeftJoin + bound-key join-variable selection | SPEC-19 ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) remaining items 1+4) | Implemented |
| [SPEC-21](SPEC-21-count-pushdown-extensions.md) | Count-pushdown extensions — equality-filter inlining, grouped COUNT, multi-count | SPEC-19 ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) remaining item 2) | Draft |
| [SPEC-22](SPEC-22-http-streaming-results.md) | Streaming SELECT results end-to-end to the HTTP layer | SPEC-19 ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) remaining item 3) | Draft |

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
