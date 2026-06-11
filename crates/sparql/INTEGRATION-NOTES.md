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
per named graph) is deferred to the named-graph epic (#7).

## HornBackend — storage/WCOJ/closure wiring (2026-06-11, #67)

`crates/sparql/src/exec/horn.rs` implements the `Executor` + `Store`
seam on top of `horndb-storage` and `horndb-wcoj`.

### Term identity and dictionary

All term identity lives in `horndb_storage::Dictionary` (kind-tagged
`TermId`s). This fixes the Stage-1 `MemStore` behaviour where terms
were stored as bare lexical strings and term kinds were recovered
heuristically from lexical shape (`classify_lexical` in `exec/mod.rs`).
Literals (leading `"`) were recovered correctly, but blank nodes were
stored as bare labels indistinguishable from IRIs and therefore surfaced
as `Term::Iri`. The dictionary's kind-tagged `TermId`s make recovery
exact for all three kinds. Two literals with the same value but different
`xsd:integer` lexical spellings (e.g. `"042"` and `"42"`) additionally
share an inline-int `TermId`; bound values decode to the canonical form.
This is closer to SPARQL value semantics than pure lexical matching.

### Tombstone deletes over insertion-only storage

`horndb-storage` is insertion-only at Stage 1. `DELETE DATA` is
implemented by a `tombstones: HashSet<(u64, u64, u64)>` overlay in
`HornBackend`. The overlay is applied when building the WCOJ snapshot:
tombstoned triples are filtered out before the sorted `VecTripleSource`
is constructed. A `stored_keys` mirror of every physically written key
gives O(1) membership tests without re-scanning the storage columns.

### Lazily-rebuilt VecTripleSource snapshot

BGP execution requires all six sort orderings (SPO, SOP, PSO, POS,
OSP, OPS). `HornBackend` builds a `VecTripleSource` lazily on the first
query after any mutation and caches it behind a `Mutex<Option<Arc<…>>>`.
The snapshot holds all six orderings eagerly sorted; at ~144 bytes/triple
steady-state snapshot cost (construction briefly peaks ~168 B/triple
while the input vec is still alive) this is a documented Stage-1 cost.
The snapshot is invalidated (set to `None`) on every write (insert or delete).

A follow-up item exists to replace this with a direct `TripleSource`
over the columnar partitions, avoiding the full-copy rebuild.

### Batched-insert core (`insert_oxrdf_batch`)

Inserting triples one at a time via `Store::insert_triple` triggers a
per-predicate partition rebuild in `horndb-storage` on each call, giving
O(n²) cost for a bulk load. `insert_oxrdf_batch` addresses this with a
read-compute / write-commit split:

1. Phase 1 (read-only): intern all terms; classify each triple as
   new-to-storage or tombstone-resurrection; collect the storage batch.
   Intern failures skip the triple (lenient for bulk loads — the
   single-triple `insert_oxrdf` propagates intern errors instead).
2. Phase 2 (write): call `store.insert_triples` once for the whole
   batch, rebuilding each predicate partition at most once.
3. Phase 3: invalidate the WCOJ snapshot once iff any triple became
   newly live.

`load_lexical_triples` and `insert_algebra_triples_bulk` both delegate
to `insert_oxrdf_batch`. The `serve` binary uses it for the initial load.

Known Stage-1 limits of the update path: HTTP `INSERT DATA` / `DELETE
DATA` (`update.rs::apply_update`) still applies triples one at a time
through the `Store` trait, so a very large update body pays the
per-call partition-rebuild cost the bulk loaders avoid — batching
`apply_update` is a candidate follow-up under the SPEC-07 epic (#7).
Likewise, a store populated via `--materialize` is not re-reasoned on
subsequent updates; incremental maintenance of the closure is SPEC-06
territory.

### `reasoner` feature and `load_with_reasoning`

The `reasoner` feature (default-on) adds a `load_with_reasoning`
function that drives the `horndb_owlrl::integration::Engine` (RuleFiring
backend) over an `oxrdf::Dataset` and loads the full materialized closure
— asserted base plus all inferred triples — into the `HornBackend` in a
single `insert_oxrdf_batch` call. GraphBLAS is not required; only the
compiled-rule RuleFiring backend is used here. The `serve` binary exposes
this path via the `--materialize` flag.

### GRAPH patterns

Named-graph patterns remain unscoped (unchanged Stage-1 behaviour).
See the GRAPH patterns section above.
