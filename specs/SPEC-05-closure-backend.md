# SPEC-05 — GraphBLAS Closure Backend

## Purpose

Handle the *closure subset* of OWL 2 RL reasoning — transitive properties, `rdfs:subClassOf`, `rdfs:subPropertyOf`, and `owl:sameAs` equivalence classes — using semiring matrix algebra on **SuiteSparse:GraphBLAS** rather than rule firing. The rule engine (SPEC-04) routes these axioms here.

This is one of the project's differentiating bets: no production OWL reasoner uses GraphBLAS as the substrate, but RedisGraph/FalkorDB prove the substrate is production-viable for graph queries.

## Scope

In scope:
- Transitive closure of any transitive property, computed as iterated `(∨, ∧)` Boolean matrix-matrix multiply (or `(min, +)` for cost-aware variants).
- `rdfs:subClassOf` and `rdfs:subPropertyOf` closures.
- `owl:sameAs` equivalence-class maintenance (union-find / EQREL-style).
- Materialization of the closure back into SPEC-02 storage as inferred triples (so SPARQL-over-closure works in the normal way).
- Integration with SPEC-04 (rule engine calls into this backend for the relevant rules) and SPEC-06 (incremental updates to the closure).

Out of scope:
- General OWL 2 RL rules — that is SPEC-04.
- SPARQL property-path closure at query time — SPEC-07 may invoke this backend on demand, but the path-evaluation algebra lives in SPEC-07.
- GPU GraphBLAS backend — SPEC-09.

## Functional requirements

**F1. SuiteSparse:GraphBLAS integration.** Link against SuiteSparse:GraphBLAS via the C ABI from Rust. Use the `GrB_*` C interface, not LAGraph (LAGraph is a higher-level library we may adopt selectively in Stage 2).

**F2. Schema matrix construction.** For each transitive property `p`, build a Boolean sparse matrix `M_p` over the subject/object ID space restricted to the predicate's extent. Subject and object IDs are dictionary IDs from SPEC-02 (densely renumbered within the predicate's extent for cache density).

**F3. Closure computation.** Compute `M_p* = I ∨ M_p ∨ M_p² ∨ … ∨ M_p^k` until fixed point, using semiring `(∨, ∧)` over GF(2)-like Boolean. Implementation: repeated `GrB_mxm` with the `GrB_LOR_LAND_BOOL` semiring, terminating when nnz stabilises.

**F4. `owl:sameAs` equivalence classes.** Maintain a union-find (`EQREL`-style; Soufflé PACT'19 reference) keyed by dictionary ID. Insertion of `?a owl:sameAs ?b` triggers union; canonical-representative selection picks the lexicographically smallest URI ID in the class. The rule engine (SPEC-04) and SPARQL planner (SPEC-07) consult this structure rather than scanning materialized `eq-*` triples.

**F5. Materialization writeback.** After closure, write inferred triples back to SPEC-02 as a single bulk insert (annotated as "GraphBLAS-derived" for provenance). Bulk-insert pathway must not re-fire rules in SPEC-04 (avoid infinite re-derivation).

**F6. Incremental update.** On insertion of a single edge into `M_p`, recompute only the affected slice of `M_p*` (forward-reachable set from new edge target, backward-reachable to new edge source). Deletion uses SPEC-06's DBSP machinery rather than DRed.

**F7. Dense renumbering cache.** Per predicate, keep a `dictionary_id ↔ dense_index` mapping for the subjects/objects appearing in that predicate. Refresh on bulk import; invalidate incrementally on updates.

## Non-functional requirements

**NF1. Transitive closure throughput.** On a transitivity chain of 25,000 nodes (Inferray benchmark shape), produce closure at ≥10 M triples/sec on the reference workstation. (Inferray: 21.3 M triples/sec on a single Intel desktop; we set the bar lower because we are paying for GraphBLAS generality.)

**NF2. `owl:sameAs` insertion latency.** O(α(n)) amortised per `sameAs` triple insertion (inverse Ackermann; standard union-find).

**NF3. Memory.** Closure matrices in CSR (or SuiteSparse hypersparse) representation; total memory for closure of all transitive properties on LUBM-8000 ≤2× the original transitive triples.

**NF4. Determinism.** Identical input produces identical output bit-for-bit (modulo blank-node renaming on the surface). GraphBLAS semiring multiplication is associative for `(∨, ∧)` so order does not matter.

## Dependencies

- SPEC-02 (dictionary, predicate-partitioned storage).
- SPEC-04 (calls in from rules `prp-trp`, `scm-sco`, `scm-spo`, `eq-*`).
- SPEC-06 (incremental delta application for closure maintenance).
- External: SuiteSparse:GraphBLAS (>=8.x).

## Acceptance criteria

1. Transitivity-chain benchmark of 2,500 nodes: faster than RDFox by ≥10× and faster than GraphDB/OWLIM by ≥50×. (Inferray reported 142× and 590× respectively; we set a looser target because our integration adds overhead Inferray does not have.)
2. SNOMED CT `subClassOf` closure (~300K classes, deep hierarchy): completes in ≤2 s on the reference workstation. (ELK does full classification in ~4 s; we are only doing closure of `subClassOf` here, a strict subset.)
3. `owl:sameAs` equivalence classes on a synthetic graph of 10M `sameAs` assertions across 1M canonical entities: union-find construction ≤5 s; canonical-representative lookup ≤100 ns average.
4. Differential test: closure-via-GraphBLAS produces the identical set of inferred triples as closure-via-SPEC-04-rule-firing (used as reference) on LUBM-100. No missing, no spurious.
5. Memory ratio on LUBM-8000 closure of transitive properties: ≤2× the original transitive-property triples.

## Risks and open questions

- **Semiring overhead for small problems.** For very small closures (e.g. a 100-node hierarchy), the GraphBLAS call overhead dominates and direct rule firing would win. Heuristic: route to SPEC-04 if `nnz(M_p) < 10⁴`; route to SPEC-05 otherwise. Threshold needs benchmark tuning.
- **Hypersparse vs CSR.** SuiteSparse picks automatically, but skewed predicates may trip pathological cases. Monitor and tune via `GxB_set` hints if needed.
- **Dense renumbering invalidation.** Frequent small inserts that introduce new IDs require renumbering; cost amortises poorly in a write-heavy workload. Stage 1 accepts re-renumbering at checkpoint boundaries only.
- **`owl:sameAs` interaction with the rule engine.** SPEC-04's `prp-*` rules see `?a owl:sameAs ?b` and infer property triples for both `a` and `b`. The rule engine must consult the EQREL structure here, not the materialized `sameAs` partition. Interface clarity matters; under-specified at this point.
- **LAGraph adoption.** LAGraph provides higher-level primitives (BFS, connected components, etc.) and may replace some of our direct `GrB_mxm` use. Stage 2 evaluation; not depended on for Stage 1.
- **GPU SuiteSparse:GraphBLAS.** Exists (Davis 2023) but is research-grade. Deferred to SPEC-09.
