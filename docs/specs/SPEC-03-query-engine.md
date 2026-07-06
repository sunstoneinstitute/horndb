---
status: draft
date: 2026-05-24
scope: "SPEC-03 — WCOJ Query Engine"
---

# SPEC-03 — WCOJ Query Engine

## Purpose

Define the join executor that all triple-pattern matching (SPARQL BGPs, rule bodies, backward-chained subgoals) flows through. This is the single hottest piece of code in the system; everything else is glue around it.

## Scope

In scope:
- Multi-way join over triple patterns using **Leapfrog Triejoin** (Veldhuizen, ICDT'14) as the primary algorithm.
- Worst-case-optimal join semantics (NPRR / AGM bound) up to a log factor.
- Vectorized execution over Arrow chunks (DuckDB-style, default chunk size 2048).
- Binary hash-join fallback for fully-ground triple patterns.
- Magic-sets / demand transformation for backward chaining (used by SPEC-07 SPARQL execution and SPEC-04 rule subgoals).
- SLG-resolution style tabling for recursive subgoals.
- Cost-based plan choice between WCOJ and binary-join for star patterns ≤3 patterns (where WCOJ overhead may not pay off).

Out of scope:
- Plan generation from SPARQL ASTs — that is SPEC-07.
- Rule firing — SPEC-04.
- Incremental delta evaluation — SPEC-06.

## Functional requirements

**F1. Triple-pattern executor.** Input: a set of triple patterns (variables and bound terms) and the index orderings available from SPEC-02. Output: a stream of variable bindings.

**F2. WCOJ on n-way patterns.** For BGPs with ≥4 triple patterns sharing variables, Leapfrog Triejoin is the default plan. For ≤3 patterns, the planner chooses between WCOJ and a binary-join tree by cardinality estimate.

**F3. Vectorization.** Bindings flow in Arrow record batches of `STANDARD_VECTOR_SIZE = 2048` rows. SIMD intrinsics (AVX-512 / NEON / SVE) used in the inner loop where applicable.

**F4. Magic-sets rewriter.** Given a query Q with bound arguments and a recursive rule set R, produce a rewritten rule set R' such that bottom-up evaluation of R' on Q yields only the demand-relevant facts.

**F5. Tabling.** Subgoal results are memoised per-query-execution; cycle detection prevents infinite recursion on cyclic rules (e.g. transitive properties).

**F6. Cardinality estimation.** Per-predicate histograms maintained by SPEC-02; planner uses them for join-order selection and WCOJ-vs-binary-join cutover.

**F7. Cancellation.** Long-running queries respond to a `Cancel` signal within 100 ms.

## Non-functional requirements

**NF1. Per-tuple overhead.** ≤5 ns/tuple in the hot path on the reference workstation (DuckDB's published baseline is ~2 ns/tuple for simpler operators; we accept 2.5× for the WCOJ trie machinery).

**NF2. Memory.** No copy of input columns — the executor operates on Arrow buffers in place. Intermediate tries are stack-allocated where possible.

**NF3. Parallelism.** Pattern execution parallelises over partitions of the leading variable. Speedup ≥0.7 × N on N cores up to memory-bandwidth saturation.

**NF4. Correctness vs binary-join reference.** Every result set produced by WCOJ is bit-identical (modulo binding order) to a reference binary-join implementation over the same inputs. Tested by differential fuzzing.

## Dependencies

- SPEC-02 (storage, six orderings, Roaring bitmaps).
- Apache Arrow (record batches).

## Acceptance criteria

1. WatDiv at SF100: query latency comparable to (within 2× of) Apache Jena with WCOJ extension (Hogan et al. ISWC'19 reports 1–2 orders of magnitude over Jena baseline; we should be in their ballpark).
2. On the 4-cycle query `(a)-p->(b)-p->(c)-p->(d)-p->(a)` over a synthetic graph of 10⁶ edges, WCOJ outperforms binary-join by ≥10× — the canonical WCOJ win case.
3. Differential fuzzer runs 100K randomly generated 2–6 pattern BGPs over LUBM-100 and finds zero mismatches against the reference binary-join executor.
4. Magic-sets-rewritten transitive query (`subClassOf+`) over the SNOMED CT TBox returns the same answer set as the materialized closure (SPEC-05), in ≤2× the wall time of a single materialized scan.
5. Cancellation: a query running over LUBM-8000 returns to the caller within 100 ms of receiving a cancel signal.

## Risks and open questions

- **WCOJ overhead on small joins.** Below ~3 patterns, the trie setup cost dominates. Planner cutover heuristic needs tuning per workload; default cutover at exactly 4 patterns may not be optimal for all shapes.
- **Tabling memory growth.** Subgoal memoisation can blow up for queries with deep recursion. We bound by configurable memory limit and spill to SPEC-02's warm tier; performance characteristics TBD.
- **Magic sets and aggregates.** Magic sets is well-understood for conjunctive queries; interaction with SPARQL aggregates and OPTIONAL is messier and may require a fallback to bottom-up materialization for those queries (acceptable for Stage 1).
- **GPU WCOJ.** cuMatch (SIGMOD'25) demonstrates GPU WCOJ but transfer cost / result materialization is non-trivial. Deferred to SPEC-09.
