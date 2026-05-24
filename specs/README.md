# HornDB Specs

This directory contains the architectural specifications for **HornDB** — a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL semantics with SPARQL 1.1 querying, designed for modern unified-memory hardware.

Specs are derived from `../initial-research.md` (the feasibility study and competitive landscape analysis). Read SPEC-00 first; the others assume it.

## Index

| SPEC | Subsystem | Status |
|------|-----------|--------|
| [SPEC-00](SPEC-00-vision.md) | Project vision & architecture | Draft |
| [SPEC-01](SPEC-01-conformance-benchmarks.md) | Conformance & benchmarking harness — **built first** | Draft |
| [SPEC-02](SPEC-02-storage.md) | Storage & dictionary encoding | Draft |
| [SPEC-03](SPEC-03-query-engine.md) | WCOJ query engine | Draft |
| [SPEC-04](SPEC-04-rule-engine.md) | OWL 2 RL rule engine | Draft |
| [SPEC-05](SPEC-05-closure-backend.md) | GraphBLAS closure backend | Draft |
| [SPEC-06](SPEC-06-incremental-maintenance.md) | DBSP incremental maintenance | Draft |
| [SPEC-07](SPEC-07-sparql-frontend.md) | SPARQL 1.1 frontend | Draft |
| [SPEC-08](SPEC-08-ml-integration.md) | ML / LLM integration boundary | Draft |
| [SPEC-09](SPEC-09-hardware-specialization.md) | Hardware specialization (Stage 3) | Roadmap |

## Reading order

- **Skim SPEC-00 first** — it names the bets, the non-goals, and the stage gating.
- **Then SPEC-01** — the harness is the bench every other spec is graded against. Built before the engine; selected subset must always be green.
- For implementers: SPEC-02 → SPEC-03 → SPEC-04 → SPEC-05 → SPEC-06 → SPEC-07 follows the dependency order.
- SPEC-08 (ML) and SPEC-09 (hardware) are optional and Stage 2/3 respectively.

## Spec format

Each spec follows roughly this structure:
- **Purpose** — one paragraph on why this spec exists.
- **Scope** — what is in and what is out.
- **Functional requirements** (F*) — what the subsystem must do.
- **Non-functional requirements** (NF*) — performance, conformance, latency targets.
- **Dependencies** — other specs and external libraries.
- **Acceptance criteria** — measurable checks for "this spec is satisfied."
- **Risks and open questions** — known unknowns; deferred decisions.

These are living documents. When acceptance criteria are met, update the status in this table. When trade-offs change, update the spec and link to the decision in the project's ADR log (TBD).
