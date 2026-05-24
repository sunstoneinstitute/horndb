# SPEC-07 — SPARQL 1.1 Frontend

## Purpose

Public query surface. Parses SPARQL 1.1 query and update strings, plans them against the underlying engine (SPEC-03 / SPEC-04 / SPEC-05), and serialises results. Implements the SPARQL 1.1 Entailment Regimes (specifically the OWL 2 RL/RDF regime) — the standardised contract that connects "SPARQL" to "reasoning."

## Scope

In scope:
- SPARQL 1.1 Query (SELECT, CONSTRUCT, ASK, DESCRIBE).
- SPARQL 1.1 Update (INSERT, DELETE, LOAD, CLEAR, DROP).
- SPARQL 1.1 Entailment Regimes — OWL 2 RL/RDF entailment regime (W3C Recommendation, March 2013).
- Property paths (the Kleene-star variants invoke SPEC-05 closure on demand).
- Standard result formats: SPARQL JSON Results, SPARQL XML Results, CSV, TSV, Turtle (for CONSTRUCT/DESCRIBE).
- SPARQL 1.1 Protocol over HTTP/1.1 and HTTP/2.
- SPARQL 1.1 Graph Store Protocol.

Out of scope:
- SPARQL 1.1 Federation (`SERVICE`) — deferred indefinitely.
- RDF 1.2 triple terms and the corresponding SPARQL surface — Stage 2 priority (tracked in `TASKS.md`). We follow W3C RDF 1.2, not the community RDF-star extension it superseded.
- GeoSPARQL — deferred indefinitely.
- SPARQL Inferencing Notation (SPIN) — out.
- Full-text search extensions — out (Stage 2 may add a Lucene/Tantivy adapter).

## Functional requirements

**F1. Parser.** Accept SPARQL 1.1 syntax per the W3C Recommendation grammar. Produce an AST. Reject malformed queries with informative error messages including line/column.

**F2. Algebra translation.** Translate AST → SPARQL algebra (BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, Group, OrderBy, Union, Minus, Service). Standard W3C-defined semantics.

**F3. Planner.** Convert algebra → physical plan. BGPs route to SPEC-03 WCOJ executor. Property paths involving Kleene-star route to SPEC-05 closure backend (materialized) or expanded via SPEC-03 magic-sets backward chaining (on-demand), planner choice based on selectivity.

**F4. Entailment regime: OWL 2 RL/RDF.** Default entailment is the OWL 2 RL/RDF regime. The planner consults SPEC-04's compiled rules to know which inferences are pre-materialized vs which require backward chaining. Two execution modes available per query:
  - **Materialized mode** (default): query against the materialized closure, no extra inference at query time.
  - **Backward-chained mode**: query the base store and invoke SPEC-03 magic-sets with the OWL 2 RL rule set as the rewrite source.
The mode is selected per-query via a non-standard pragma or globally by configuration; default is materialized.

**F5. Update.** INSERT/DELETE/LOAD/CLEAR/DROP. Each update is a single SPEC-06 delta operation. Subject to MVCC snapshot semantics — concurrent readers see the pre-update state until commit.

**F6. Result serialization.** Stream results — never buffer the entire result set in memory. Backpressure via the underlying HTTP/2 stream where the protocol supports it.

**F7. SPARQL Protocol.** Embedded HTTP server (axum or hyper-based) exposing the standard `/query` and `/update` endpoints. Authentication is out-of-scope at the spec level; a reverse-proxy or sidecar pattern is assumed.

**F8. Property paths.** Implement all SPARQL 1.1 path operators (`/`, `^`, `|`, `?`, `+`, `*`, `!`). For `*` and `+` with reasoning enabled, the planner is allowed to invoke SPEC-05 closure (and materialize the result of the path query) rather than evaluate the path natively, when selectivity favours it.

**F9. EXPLAIN.** Non-standard `EXPLAIN` pragma returns the chosen physical plan, estimated cardinalities, chosen indexes, and execution mode (materialized vs backward).

## Non-functional requirements

**NF1. Query latency.** On LDBC SPB SF3 read workload, geometric-mean query latency ≤2× GraphDB Enterprise on the same hardware.

**NF2. Update throughput.** ≥10K simple-INSERT statements/sec sustained on a warm LUBM-8000 store (with full reasoning maintained via SPEC-06).

**NF3. Parser throughput.** ≥10K queries/sec for the SPB query mix (parse + plan, no execution).

**NF4. Concurrent queries.** Engine supports ≥256 concurrent in-flight queries on the reference workstation with linear or sub-linear latency degradation up to memory-bandwidth saturation.

**NF5. Standards conformance.** 100% pass on the W3C SPARQL 1.1 Query test suite and the SPARQL 1.1 Entailment Regimes test suite for the OWL 2 RL/RDF regime. (Tested via SPEC-01 harness.)

## Dependencies

- SPEC-03 (executor).
- SPEC-04 (rule set, used by entailment regime).
- SPEC-05 (closure, for property paths).
- SPEC-06 (updates, delta).
- External crates: a SPARQL parser (consider `oxigraph`/`spargebra` for grammar, but consume as library only — we are not adopting Oxigraph's storage layer).

## Acceptance criteria

1. 100% pass on W3C SPARQL 1.1 Query Test Suite, default-entailment ("simple") regime.
2. 100% pass on W3C SPARQL 1.1 Entailment Regimes test suite, OWL 2 RL/RDF regime.
3. LDBC SPB SF3 read workload: geometric-mean latency ≤2× GraphDB Enterprise on identical hardware.
4. Sustained 10K simple-INSERT/sec on warm LUBM-8000 with full SPEC-06 incremental maintenance running.
5. EXPLAIN on a representative recursive query (`subClassOf+ * 5`) clearly shows the chosen mode (materialized vs backward) and cardinality estimates.
6. Differential test: same query under materialized mode vs backward-chained mode returns identical result sets on LUBM-100.
7. Property-path query `?x rdfs:subClassOf* :Person` over the SNOMED CT TBox returns within 1 s.

## Risks and open questions

- **Property paths × reasoning interaction.** This is the area the research called out as "RDFox has spent years on; under-specified in the standard." Specifically: how does a `*` property path interact with OWL 2 RL inferences over the same predicate? We follow the W3C entailment-regime spec literally; edge cases may surface only against the conformance suite.
- **MINUS + entailment.** SPARQL MINUS semantics under entailment are subtle (set difference over an inferred answer set). Test coverage in W3C suite is thin; we will discover edge cases.
- **OPTIONAL + magic sets.** Magic-sets rewriting is well-understood for conjunctive queries but messier for OPTIONAL (LeftJoin) and aggregates. Plan B: fall back to materialized-mode execution for queries containing OPTIONAL or aggregate (loses some speedup, preserves correctness).
- **Parser choice.** Writing a SPARQL parser is multi-week work we do not want to redo. `spargebra` (the parser from Oxigraph) is permissively licensed and high-quality; we adopt it as a library boundary. Long-term, if parser quirks block us, we have the option to fork.
- **Federation (`SERVICE`).** Repeatedly requested by users in practice. Deferred but the data model leaves room.
- **Protocol versioning.** SPARQL 1.2 is in draft (as of research date). We track but do not implement.
