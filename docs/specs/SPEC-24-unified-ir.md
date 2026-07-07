---
status: draft
date: 2026-07-07
scope: "umbrella: a single logical IR expressing query AND reasoning, so the optimizer can jointly decide join order, reasoning strategy (materialize vs. rewrite vs. delegate), and demand-driven partial closure — subsumes SPEC-23 and pulls in SPEC-03 F4/F5 + SPEC-07 backward mode"
---

# Unified query + reasoning IR — design (STUB)

> **This is a `to-spec` stub, not a finished spec.** It captures the decision and
> the design seed; the full spec (with Functional/Non-functional requirements and
> Acceptance criteria) is the deliverable of epic **E1**
> ([#185](https://github.com/sunstoneinstitute/horndb/issues/185)). Do not treat
> the sections below as normative until this frontmatter reaches `status: approved`.

**Refines / subsumes:** SPEC-23 (optimizer framework — becomes a *component* of
this IR), SPEC-03 F4/F5 (magic-sets / SLG tabling), SPEC-07 (backward-chained
entailment mode). Consumes SPEC-04/05/11 (the reasoning subsystems) as delegate
targets and SPEC-08 F2 (`PlanAdvisor`).
**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

## Problem (the decision this spec records)

HornDB today **materializes the full OWL 2 RL closure up front** (the canonical,
PTIME-tractable way to serve OWL 2 RL) and then runs SPARQL as pure pattern
matching over the closed graph. Reasoning-strategy selection — compiled OWL-RL
rule vs. GraphBLAS closure resolver, SSSOM crosswalk expansion, SKOS hierarchy
resolution — is fixed at **rule-compile / materialization time**
(`rules.toml` `delegate = "closure"`, the SPEC-11 crosswalk index), *upstream* of
the query optimizer. See `docs/architecture.md` §15 "Query optimization vs.
reasoning-strategy selection".

That separation leaves joint optimizations on the table. Once hybrid
backward-chaining exists (a query can answer without full materialization), the
engine faces a real per-subgoal choice — **materialize vs. rewrite vs.
delegate-to-resolver** — and the natural place to make it is a **single logical IR
that expresses query and reasoning together**, where the optimizer can push
filters through rule expansions, order joins across base and inferred patterns,
and compute only the closure slice a query reaches.

## Design seed (to be developed)

- **One logical IR** = the SPEC-23 logical IR (flat n-ary BGP, binding/type
  lattice, pass registry, `Stats` seam), extended so **reasoning enters as
  first-class rewrite passes + delegate nodes** — *not* as generic recursive
  query patterns a cost optimizer grinds on.
- **Heavy recursion still delegates.** Transitive closure hands off to the
  specialized GraphBLAS operator (SPEC-05) via a `ClosureScan`/`PathClosure`
  (`Algebra::l`) node the optimizer *chooses* but does not try to out-plan with
  join reordering. This is the hybrid: materialize/​delegate the closure subset,
  rewrite/backward-chain the rest.
- **Reasoning/materialization catalog seam**, parallel to `Stats`: what is already
  closed + the cost of each resolver, so the materialize-vs-rewrite-vs-delegate
  choice is cost-based on the same additive scale as join ordering.
- **Magic-sets / demand transformation** (SPEC-03 F4) + **SLG tabling** (F5) are
  the machinery that generates the query-driven rewrites; **SPARQL backward mode**
  (SPEC-07) is the surface.
- **Prior-art blind spot:** the SPEC-23 surveys (Oxigraph `sparopt`, DuckDB,
  ClickHouse) are all non-reasoning engines — this layer is HornDB-specific and
  has no borrow-from-X answer.

## Phasing (indicative — firm up when spec'ing)

1. Land the SPEC-23 framework (logical IR + pass registry + `Stats`) — the
   foundation; already specified in SPEC-23.
2. Reasoning-as-rewrite passes + the reasoning/materialization catalog seam.
3. Magic-sets / demand transformation + SLG tabling (SPEC-03 F4/F5).
4. Cost-based materialize-vs-rewrite-vs-delegate; property-path → GraphBLAS
   selectivity choice (SPEC-07 F3 fast path).
5. SPARQL backward-chained entailment mode (SPEC-07); ML `PlanAdvisor` validation.

## Open questions

- Does SPEC-23 stay a standalone spec (component) or get folded wholesale into
  SPEC-24? (Current plan: SPEC-23 stays; SPEC-24 references it.)
- The wcoj↔sparql planner API boundary (also a SPEC-23 §8 open question).
- Termination / cost / cardinality of recursive fixpoints in a cost model that
  otherwise assumes non-recursive AGM/hash costing.
