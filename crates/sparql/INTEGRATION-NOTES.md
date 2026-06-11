# SPEC-08 Integration Notes for `horndb-sparql`

These notes describe call sites that **SPEC-07's plan** is responsible
for implementing.

## F2 — PlanAdvisor at the SPARQL planner

Same contract as `wcoj/INTEGRATION-NOTES.md` — the SPARQL planner
constructs a `SubplanShape` from its algebra tree, calls
`registry.plan_advisor().advise(&shape)`, validates against its own
histograms, and falls back if implausible. NF2's 1 ms p99 budget
applies here too.

## F5 — Filtering by provenance in SPARQL

SPARQL queries should be able to filter on the provenance column
exposed by SPEC-02. SPEC-07's plan should:

1. Recognise the (engine-specific) predicate
   `<https://horndb.io/prov/source>` in `FILTER`
   clauses.
2. Map literal values `"symbolic"` and `"ml-derived"` onto the
   `MlProvenance` discriminants from SPEC-02's storage column.
3. Allow audit queries of the form:
   ```sparql
   SELECT ?s ?p ?o ?model WHERE {
     ?s ?p ?o .
     ?s <https://horndb.io/prov/source> "ml-derived" .
     ?s <https://horndb.io/prov/model>  ?model .
   }
   ```

## F3 — LLM → SPARQL endpoint (STAGE 2 — DEFERRED)

`POST /nl-query` is **not** part of Stage 0/1. When SPEC-07's plan
adds it, the implementation should:

1. Live in a new module (`crates/sparql/src/nl.rs`).
2. Take an injected `Arc<dyn LlmClient>` (trait to be defined in
   `horndb-ml` Stage 2) so the LLM provider is pluggable and the
   handler is testable without network.
3. Always return the generated SPARQL alongside the results (per
   SPEC-08 risks: "LLM SPARQL quality").
4. Defer cost reporting and training-data leakage controls to
   Stage 2+ per SPEC-08.

For Stage 0/1 the file remains absent — `horndb-ml` ships only
the boundary; the LLM client trait will land with the Stage 2 plan.

## GRAPH patterns (Stage 1, #66)

`GRAPH <iri> { P }` and `GRAPH ?g { P }` lower transparently to `P`.
The Stage-1 executor holds a single merged graph (corpora are loaded
from flat triple dumps), so there is no named-graph store to scope
against; a graph-name variable remains unbound in results. This makes
the SPB named-graph queries (Q10/Q12) translate and run. Correct
named-graph scoping (zero solutions for absent graphs, `?g` binding
per named graph) is deliberately deferred to the storage wiring
increment (#67), where quads exist.
