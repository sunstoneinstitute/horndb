---
status: draft
date: 2026-05-24
scope: "SPEC-04 — OWL 2 RL Rule Engine"
---

# SPEC-04 — OWL 2 RL Rule Engine

## Purpose

Define how the OWL 2 RL/RDF rule set (W3C OWL 2 Profiles spec, Tables 4–9) is applied to the store. This is the forward-chaining engine — the source of inferred ("materialized") triples in the persistent store.

## Scope

In scope:
- The complete OWL 2 RL/RDF rule set (the ~80 rules in Tables 4–9 of the OWL 2 Profiles normative spec).
- **Ahead-of-time compilation** of the rule set to native Rust (Soufflé-style). No rule interpreter in the hot path.
- Semi-naïve evaluation with delta tables.
- Proof recording (each inferred triple records its rule and premises) for provenance.
- Stratification of the rule set (negation-free, so trivially stratified; check is mechanical).
- Coordination with SPEC-05 (closure subset routed to GraphBLAS) and SPEC-06 (delta integration).

Out of scope:
- Custom user-defined rules (Datalog dialect) — Stage 2 extension.
- Existential rules / tuple-generating dependencies — out indefinitely.
- OWL 2 EL / OWL 2 QL profiles — separate engines if needed at all.
- Backward chaining of the rule set — that is SPEC-03 + SPEC-07 (using the same compiled rules in a magic-sets context).

## Functional requirements

**F1. OWL 2 RL/RDF rule set.** All rules from the normative tables: `eq-*` (equality), `prp-*` (properties), `cls-*` (classes), `cax-*` (class axioms), `scm-*` (schema), `dt-*` (datatypes). Compliance is measured by passing the W3C OWL 2 Test Cases (SPEC-01).

**F2. Codegen pipeline.** A static Rust crate `owlrl-rules` is generated at *build time* from the rule specification. Each rule becomes a function that takes the relevant predicate-partitions (from SPEC-02) and a delta and returns a new delta. The compiled binary contains no rule interpreter.

**F3. Semi-naïve evaluation.** At each iteration, compute `Δ_{i+1} = T(F_i ∪ Δ_i) \ F_i` where `T` is the rule operator. Terminate when `Δ = ∅`. Use SPEC-05 for the schema/closure subset; SPEC-06 for the delta machinery.

**F4. Proof recording.** Each inferred triple `t` carries (or can be queried for) a tuple `(rule_id, premise_triple_ids[])`. Stored as a compressed side-table; not all triples need to retain proof at runtime, but the system can re-derive any proof on demand by running the relevant rule body as a backward query (SPEC-03).

**F5. Skew handling on `rdf:type`.** Rules whose body contains `?x rdf:type ?C` (which is most of `cls-*` and `cax-*`) must avoid serial scans over the entire `rdf:type` partition; the compiled rule body partitions work by class ID and parallelises.

**F6. owl:sameAs equivalence classes.** Routed to SPEC-05's EQREL-style equivalence-class structure; the rule engine itself does not re-derive `eq-sym`, `eq-trans` triples — they are implied by the equivalence-class representation.

**F7. Reset and rematerialize.** On demand, drop all inferred triples and rerun forward chaining from the asserted base. Used for debugging, schema migration, and the conformance test harness.

## Non-functional requirements

**NF1. Materialization throughput.** ≥2 M triples/sec on LUBM-8000 forward-chaining the full OWL 2 RL rule set, single-node reference workstation. (RDFox: 6.1 M triples/sec on SPARC T5-8/128-core/4 TB RAM, ISWC 2015 paper; we target ≥1/3 of that on much smaller hardware as a first-cut goal.)

**NF2. Expansion ratio.** Storage budget for materialized triples is ≤4× the asserted set on OWL 2 RL workloads (GraphDB: 1:3.2 expansion ratio on SPB-256 is the published baseline).

**NF3. Rule firing latency.** Time from "asserted-triple inserted" to "all derivable consequences materialized" on a steady-state warm store: ≤1 second for a single-triple insertion against an LUBM-1000-sized store. (Steady state — initial bulk materialization is a separate cost.)

**NF4. Proof retrieval.** Producing a proof tree for a single inferred triple is O(depth × per-rule-cost) and must complete within 100 ms for proofs of depth ≤10 on the reference workstation.

## Dependencies

- SPEC-02 (storage).
- SPEC-03 (joins inside rule bodies).
- SPEC-05 (closure subset).
- SPEC-06 (delta machinery).

## Acceptance criteria

1. All W3C OWL 2 RL conformance tests pass (SPEC-01 dependency).
2. LUBM-8000 full materialization completes in ≤10 minutes on the reference workstation (~1.8 M triples/sec materialization rate after subtracting GraphBLAS-handled closure).
3. Expansion ratio on LDBC SPB-SF3 OWL 2 RL run: ≤4× (baseline GraphDB published 1:3.2).
4. Differential test: full re-materialization from base after `Reset` produces a bit-identical store (modulo blank-node renaming).
5. For any inferred triple `t`, a `proof(t)` call returns a tree whose leaves are asserted triples and whose internal nodes are rule applications that derive `t`. Tested against W3C explanation-test fixtures (in conjunction with SPEC-01).

## Risks and open questions

- **Rule-codegen complexity.** Soufflé's codegen is ~50K lines of C++ and represents serious engineering effort. We are not re-implementing Soufflé; we are emitting Rust for ~80 well-known rules. Risk: under-budgeting this and ending up with an interpreter "for now."
- **Datatype reasoning (`dt-*` rules).** OWL 2 datatype reasoning (xsd numeric tower, decimal precision, dateTime canonicalisation) is non-trivial. Stage 1 supports `xsd:int`, `xsd:string`, `xsd:dateTime`; full datatype map is a Stage 2 deliverable.
- **Skew on `rdf:type`.** Naïve parallelism over `rdf:type` is the canonical performance killer in this class of engines. The partition-by-class-id strategy is sound on paper but unproven in our codebase; budget time to tune.
- **Rule ordering / fixed-point detection.** Naïve fixed-point checks are expensive. Strategy: per-rule "dirty" flags driven by SPEC-06 delta annotations. Borrowed from Soufflé.
- **User rules.** A future Datalog frontend would let users write custom rules; this requires re-introducing a (small) rule compiler at runtime. Out of scope for Stage 1.
