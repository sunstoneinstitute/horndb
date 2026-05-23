# SPEC-08 — ML / LLM Integration Boundary

## Purpose

Define where machine learning (embeddings, classical ML models, LLMs) sits in the system architecture. The principle is non-negotiable: **the symbolic reasoner is the source of truth and proof; ML is a performance optimizer and a candidate generator.** Embedding-based "neural reasoning" is not a substitute for OWL 2 RL entailment in a provenance-oriented system.

This spec is deliberately small. We want optionality, not a parallel inference stack.

## Scope

In scope:
- A plugin interface for ML models that *propose* candidate `owl:sameAs` links, candidate type assignments, candidate property assertions. Every proposal must be re-verified by symbolic reasoning before being committed to the store.
- A plugin interface for ML models that *advise* the query planner (cost estimates, join-order suggestions). Wrong advice degrades performance but cannot affect correctness.
- An LLM → SPARQL translation layer (Stage 2): natural-language questions arrive over an HTTP endpoint, are translated to SPARQL by an LLM, executed by SPEC-07, and results are returned alongside the generated SPARQL for inspection.
- A "predicted hot set" advisor for tier placement (SPEC-02): a model that predicts which triples will be queried in the near future, biasing tier placement.

Out of scope:
- Knowledge-graph embedding training as a product feature. Sunstone uses external embedding pipelines if needed; this engine does not train them.
- Neural-symbolic hybrid reasoning that bypasses OWL 2 RL semantics. Out indefinitely.
- LLM agent loops that operate inside the engine. The LLM lives at the boundary.

## Functional requirements

**F1. Candidate-link plugin interface.** A Rust trait `CandidateGenerator` exposes `propose_sameas(left: TripleSubject, right: TripleSubject) -> Confidence`. Implementations may use embeddings (FAISS / Vespa / custom), feature-engineering, or rules. The engine treats every proposal as a hypothesis: it is added to a `staging.sameAs` graph, runs through SPEC-05 to compute the implied equivalence-class consequences, and either commits or rolls back based on a configurable policy (auto-commit above confidence X; queue for human review otherwise).

**F2. Planner advisor interface.** A trait `PlanAdvisor` returns `(estimated_cardinality, suggested_index, suggested_join_order)` for a given subplan. The planner (SPEC-03 / SPEC-07) treats this as a hint; it always validates against its own histograms and falls back if the hint is implausible.

**F3. LLM → SPARQL endpoint (Stage 2).** HTTP endpoint `POST /nl-query` accepts `{question, schema_hint?, model_id?}` and returns `{generated_sparql, results, confidence, explanation}`. The generated SPARQL is **always** included so users can audit. The LLM call is delegated to an external model via API — the engine does not bundle a model.

**F4. Hot-set advisor interface.** A trait `HotSetAdvisor` returns, periodically, a set of triple-IDs predicted to be queried frequently in the next window. SPEC-02's tiering uses this as an input to its placement policy (alongside actual recent-access statistics).

**F5. Provenance for ML-derived facts.** Any triple committed to the store via an ML-driven path carries a `prov:wasDerivedFrom` annotation referencing the candidate-generator's identity and confidence. Distinguishable from purely-symbolic-derived triples by the SPARQL planner so that audit queries can filter.

**F6. Audit endpoint.** `GET /ml-audit?since=...` returns a paginated list of ML-derived facts admitted in the time window, with confidences and the generating model identity.

## Non-functional requirements

**NF1. No correctness impact.** Disabling all ML plugins (configuration flag `ml.enabled = false`) produces bit-identical query results compared to ML-enabled mode (modulo the absence of ML-proposed `sameAs` facts in the store). Tested via differential harness.

**NF2. Planner-advisor latency.** Plan-advice call adds ≤1 ms to planning time, p99. Above 1 ms, the planner skips the advisor for that query and logs a warning.

**NF3. Candidate-generator throughput.** Whatever rate Sunstone's external embedding pipeline produces, the staging-graph admission path can absorb at ≥10K candidates/sec on the reference workstation.

**NF4. LLM endpoint latency.** Dominated by upstream LLM API call; engine-side overhead ≤50 ms p99 for parsing, validation, execution, and serialisation.

## Dependencies

- SPEC-02 (provenance annotations on triples; tier-placement input).
- SPEC-03 (planner advisor).
- SPEC-04/SPEC-05 (re-verification of candidate facts).
- SPEC-07 (NL-query endpoint piggybacks on SPARQL).

Not depended on by anything in the critical correctness path. The whole spec is opt-in via configuration.

## Acceptance criteria

1. With `ml.enabled = false`, full SPEC-01 conformance suite passes — proving the ML path cannot affect correctness.
2. A reference `CandidateGenerator` implementation (embedding-based, FAISS in-process) integrated end-to-end on a synthetic person-entity-resolution dataset: ≥10× speedup over a brute-force scan for finding candidate `sameAs` links, while symbolic re-verification correctly rejects ≥99% of false positives.
3. NL-query endpoint operational on a small SPARQL test set (Stage 2): generates valid SPARQL on ≥80% of queries from a curated 100-question benchmark; users always see the generated SPARQL.
4. Audit endpoint returns every ML-derived fact with the source model ID and confidence; can drive a UI for human review.
5. Disabling/re-enabling ML at runtime via configuration reload does not require an engine restart.

## Risks and open questions

- **Confidence calibration.** Auto-commit thresholds depend on calibrated confidence scores from the candidate generator. Most embedding models are uncalibrated. Stage 1 defaults to "always queue for human review" — auto-commit is opt-in per-deployment.
- **`sameAs` cascade.** A single proposed `sameAs` can merge two equivalence classes and trigger a flood of inferred property triples. SPEC-05 handles this efficiently, but rollback after a wrong proposal is expensive — we must verify the proposal before computing consequences when policy is "auto-commit." For "queue for review," the proposal lives in staging until accepted.
- **LLM SPARQL quality.** LLMs are inconsistent at generating SPARQL, especially for property paths and entailment regimes. The endpoint should *always* return the generated SPARQL so users can correct it; auto-execution of low-confidence translations is dangerous.
- **Cost transparency.** LLM API costs are user-borne; the engine should report per-query cost (token-counts) so users can budget.
- **Training-data leakage.** If we host an NL-query endpoint, queries may be logged by the upstream LLM provider. Document this and offer a configuration knob to disable logging.
- **Privacy / GDPR.** Embedding-based candidate generation may incidentally memorise PII from the store. Document.
- **Boundary discipline.** The temptation to push more ML into the engine is real; every such push erodes the provenance guarantee. This spec's purpose is to draw a sharp line and hold it. Future SPECs that propose moving the line must justify against this one.
