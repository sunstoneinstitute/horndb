---
status: draft
date: 2026-07-06
scope: "a single logical IR expressing query AND reasoning — logical IR, pass registry, statistics seam, cost-based ordering, and (later) reasoning-as-rewrite + magic-sets/backward-chaining — across the WCOJ + SPARQL planners; refines SPEC-03 (F2/F4/F5/F6) and SPEC-07"
---

# Unified query + reasoning IR (incl. optimizer framework) — design

**One-line thesis:** one logical IR carries both the query and the reasoning, so
the optimizer can decide join order **and** reasoning strategy
(materialize / rewrite / delegate) on a single comparable cost scale. The
optimizer framework (§5.1–5.7) is the foundation that ships first; reasoning
enters the same IR as (later) rewrite passes and delegate nodes (§5.8).

**Refines / subsumes:** SPEC-03 (WCOJ query engine — F2/F6 now, F4 magic-sets and
F5 SLG tabling later) and SPEC-07 (SPARQL frontend — including its backward-chained
entailment mode); consumes SPEC-04/05/11 (the reasoning subsystems) as delegate
targets and SPEC-08 F2 (`PlanAdvisor`) and the not-yet-built SPEC-02 statistics
surface. This spec absorbs what was briefly split out as "SPEC-24"; there is one
unified-IR spec, not two.
**Epic:** [#185](https://github.com/sunstoneinstitute/horndb/issues/185).

## Problem

HornDB has two planners, both explicit Stage-1 stubs, and no optimizer *framework* to
hang real logic on:

- **WCOJ planner** (`crates/wcoj/src/planner.rs`, `plan.rs`). `Planner::choose` is a
  pattern-count cutover: `< wcoj_cutover` (default 4) → `BinaryHash`, else `Wcoj`. It
  takes a `&C: Cardinality` estimator argument and **ignores it** (`_est`). Variable
  order for the trie is picked by descending *degree* (patterns mentioning the var),
  not by any cardinality signal (`ExecutionPlan::for_bgp`, `plan.rs:42-57`).
- **SPARQL planner** (`crates/sparql/src/plan/planner.rs`). A thin 1:1 `Algebra →
  PhysicalPlan` lowering. No cost model; BGP patterns pass to the executor in textual
  order, and join ordering is delegated downward to the WCOJ layer.
- **Cardinality seam** (`crates/wcoj/src/cardinality.rs`). A `Cardinality` trait with
  one impl, `UniformEstimator`: each bound position multiplies selectivity by `1/16`
  over the total triple count. Deliberately coarse; the docstring points at "Stage-2
  histograms from SPEC-02" that do not exist yet.
- **ML advice seam** (`crates/ml/src/planner.rs`). A `PlanAdvisor` trait
  (`advise(&SubplanShape) -> PlanAdvice`) with a `DisabledPlanAdvisor` no-op default.
  The integration contract (SPEC-08 F2, `crates/wcoj/INTEGRATION-NOTES.md`) says the
  symbolic planner is source of truth: advice is a *hint*, validated against the
  planner's own statistics and discarded if implausible, and skipped if it blows a 1 ms
  p99 budget.
- **EXPLAIN** already renders per-node `~N rows` estimates (`plan::explain`,
  `Executor::cardinality_estimate`), so there is a rendering surface waiting for real
  estimates.

The gap is not "add a cost model" — it is that there is no *place* to add one. There is
no logical IR distinct from the physical plan, no pass pipeline, no statistics provider
seam on storage, and no cost model. SPEC-03 F6 ("planner uses per-predicate histograms
for join-order selection and WCOJ-vs-binary-join cutover") is unimplementable until that
scaffolding exists. This spec defines the framework; concrete estimators, statistics,
and orderings land against it in phases (§6).

## Non-goals

- **A Cascades/Volcano memo or transformation-rule search engine.** None of the three
  systems surveyed (Oxigraph, DuckDB, ClickHouse) use one; all three run a *fixed,
  hand-ordered pass pipeline*. We do the same. Plan-space search is confined to the one
  place it pays — join/variable ordering (§5.5) — as a dedicated stage, not a rewrite
  rule.
- **Histogram-driven estimation in v1.** DuckDB's published result ("Join Order
  Optimization with (Almost) No Statistics") is that base cardinalities + per-column
  distinct counts (NDV) get competitive orders without histograms. We adopt NDV +
  counts first; quantile/count-min sketches are a later phase (§6) behind the same seam.
- **Changing WCOJ row production or the storage physical layout.** The optimizer emits
  a plan; it does not touch `executor/wcoj.rs`'s `BatchIter` or SPEC-02 partitions.
- **A new statistics maintenance subsystem in this spec.** We define the *provider seam*
  the optimizer reads from and the minimal counts it needs; who populates it (and its
  interaction with the SPEC-06 DBSP delta model) is called out as a dependency, not
  designed here.
- **Retiring `UniformEstimator` or the pattern-count cutover immediately.** They become
  the zero-statistics fallback behind the new seam, kept until a stats-backed estimator
  is proven at least as good on the harness.

## Prior art (what we borrow, what we reject)

Three engineering briefs were commissioned (Oxigraph `sparopt`, DuckDB, ClickHouse
Analyzer); full source references are in the per-system notes below. Distilled:

### Oxigraph `sparopt` — the RDF-native prior art (already a transitive dep)

- **Purely rule-based, single-shot, no fixpoint, no real statistics.** Three passes:
  `normalize` (type-aware simplification), `reorder_joins` (greedy), `push_filters`.
- **Cardinality is a static 8-entry table keyed on the bound/unbound *shape* of a
  triple pattern** — bound subject ≈ 100, bound predicate *alone* ≈ 1,000,000 (predicate
  is unselective), etc. Used only to break ties in join ordering; never touches data.
- **Join ordering is greedy seed-and-grow:** pick the smallest-estimate pattern, then
  repeatedly add the cheapest remaining pattern that *shares a variable* (avoids
  Cartesian products), with a `size(l)*size(r)/1000^keys` cost.
- **The one reusable abstraction is the `VariableType` binding/type lattice**
  (`type_inference.rs`): a 5-bit lattice `{undef, named_node, blank_node, literal,
  triple}` threaded through every pass. "Bound" is *derived* (a var is bound iff its
  `undef` flag is false). This single fact powers filter-pushdown legality, join-key
  discovery, and an `Equal → SameTerm` strength reduction.
- **Smart constructors** (`join`/`filter`/`union_all`) fold empties/identities/constant
  filters at build time, keeping the algebra canonical so passes skip trivial cases.

  **Borrow:** the binding/type lattice, smart constructors, the greedy connected
  seed-and-grow as a *fallback*, the `Equal→SameTerm` and filter-pushdown-legality
  rewrites, single-shot pipeline.
  **Reject:** the nested-binary-`Join` BGP representation. `sparopt` has *no* n-ary BGP
  node — a k-triple BGP is a tree of binary `Join`s it flattens and rebuilds to reorder.
  A WCOJ engine wants the opposite (§5.1): a **flat set of triple patterns** whose plan
  object is a *global variable elimination order*, not a binary join tree.

### DuckDB — the cost-based join-order reference

- **~39 hand-ordered logical passes**, each a class taking a `LogicalOperator` tree and
  returning one, invoked via `RunOptimizer(OptimizerType::X, lambda)` — a **typed,
  named, individually disable-able pass registry** with debug plan-verification after
  each pass. This legibility is the headline borrow.
- **Filter *pull-up then push-down*** (hoist all filters so a combiner can derive
  transitive/constant predicates, then push the enriched set to the scans).
- **Statistics = base row counts + HyperLogLog NDV per column + min/max**, ranked by
  confidence. Cardinality = `∏ base / denominator`, denominator built per join edge as
  `|A||B| / max(NDV_a, NDV_b)`, with **transitive-equality-class tracking** (a key across
  ≥3 relations is not double-divided) and a **PK/FK cap** (a key-join output never
  exceeds the smaller side). Sub-plan estimates memoized by relation bitset.
- **Join ordering = DPhyp** (Moerkotte/Neumann) over the query hypergraph — enumerates
  connected-subgraph/complement pairs, so it never considers a Cartesian product unless
  the graph forces it. **Cost is cardinality-dominated and additive** (`card(result) +
  cost(left) + cost(right)`), i.e. minimize total intermediate materialization.
- **Dual scaling guard:** relation-count threshold *and* a ~10 000-pair work budget →
  fall back to **greedy operator ordering**.
- **Logical/physical split:** the optimizer emits *logical*; a separate
  `PhysicalPlanGenerator` picks operators. Join *order* and hash *build-side* are decided
  logically (build-side in a late pass); join *algorithm* physically.

  **Borrow:** the pass registry shape, NDV+counts estimator with the transitive-class
  and PK/FK tricks, additive cardinality-dominated cost, DP-small/greedy-large with an
  explicit threshold, logical/physical separation, bitset-keyed memoization.
  **Reject wholesale-copying the binary DP for the WCOJ portion:** DuckDB's DP searches
  *binary join trees* and costs pairwise intermediates. Leapfrog triejoin over a full
  BGP has **no binary intermediates**; its cost is the AGM/fractional-edge-cover bound
  and its plan is a *variable order*. DP over binary trees is the wrong search for the
  WCOJ sub-plan (§5.5).

### ClickHouse — the columnar, rule-first, "cost-based added last" reference

- **Two IRs:** a resolved **Query Tree** (~46 semantic passes, `IQueryTreePass`,
  validate-after-each-pass) and a physical **Query Plan** (4 numbered optimization
  passes over the step tree). "Resolve names/types *once* upfront, then rewrite freely" —
  the rebuild that replaced in-place AST rewriting, which had capped their feature growth.
- **Heuristic-first; genuinely cost-based only for join order (25.9+).** A decade of
  world-class OLAP with essentially no join reordering — the wins came from *not reading
  data*: sparse primary-key granules, skip indexes (min/max, bloom, set), and **PREWHERE**
  (read filter columns first, materialize survivors, order filters by *ascending
  uncompressed size* as a zero-stats cost proxy).
- Column statistics are opt-in sketches: **TDigest** (quantiles), **Uniq** (HLL),
  **CountMin** (equality selectivity), **MinMax** (part pruning). Join reordering uses
  them + row counts with **DP-for-small / greedy-for-large** — the same split as DuckDB.
- **Runtime filters / join rewrites** transfer directly: bloom filter built from the
  build side pushed into the probe scan (`tryAddJoinRuntimeFilter`), any-join →
  semi/anti, join → `IN`.

  **Borrow:** the resolve-once logical IR, per-pass toggle + validate-after-each-pass,
  the "don't-read-data" family (skip-index / PREWHERE analogs on dictionary-encoded
  partitions, ordered by uncompressed size), sideways-information-passing runtime filters,
  and the discipline of shipping heuristics first.
  **Reject / caution:** hand-ordered pipelines rot (their source is littered with "must
  run before X" comments and the Analyzer flip broke correctness in the field) — encode
  pass ordering constraints *explicitly*, not as tribal comments. And do **not** try to
  express join/variable ordering as "just another rewrite pass"; give it its own
  cost-based stage.

### Cross-cutting conclusions

1. **Fixed hand-ordered pass pipeline, not a memo search.** All three converge here.
2. **A resolved logical IR separate from the physical plan**, with a binding/type
   lattice threaded through — Oxigraph's lattice + ClickHouse's resolve-once.
3. **NDV + counts, no histograms, in v1.** DuckDB's evidence; ClickHouse's sketch menu
   is the later-phase upgrade path.
4. **Ordering is HornDB's one place cost matters — and it is bimodal.** ClickHouse could
   ignore join order for a decade; a WCOJ engine *cannot* (variable order *is* the plan
   and is exponentially sensitive). But the plan object differs by branch: a **variable
   elimination order** for the WCOJ sub-plan (AGM-aware), a **binary join tree** for the
   hash-fallback sub-plan (DuckDB-style DP/greedy). The optimizer must choose *between*
   the two and cost them on one comparable scale.
5. **Typed, individually toggleable passes with post-pass validation** — invaluable for
   bisecting plan regressions against the conformance harness.

## Design

### 5.1 Plan IR — flat BGP, resolved once

Introduce a logical IR in `horndb-sparql` (`crates/sparql/src/plan/logical.rs`) distinct
from the existing `PhysicalPlan`. The critical departure from `sparopt`: **the BGP is a
flat n-ary set of triple patterns**, not a tree of binary joins. The plan object for a
BGP is not a join tree — it is a `JoinSpec` (§5.5) that the physical lowering realizes as
either a WCOJ variable order or a binary-hash tree.

```
LogicalPlan =
  | Bgp { patterns: Vec<TriplePattern> }            // flat, n-ary — the WCOJ unit
  | Join { left, right }                            // algebra join of non-BGP subtrees
  | LeftJoin { left, right, expr }
  | Filter { expr, inner }
  | Union | Project | Distinct | Slice | OrderBy | Extend | Values | Group | PathClosure
```

Contiguous algebra `Join`s over `Bgp`/`QuadPattern` leaves are **coalesced into one
flat `Bgp`** on entry (the inverse of `sparopt`'s flatten-and-rebuild, done once), so the
WCOJ planner sees the widest possible pattern set to order globally.

**Binding/type lattice** (`crates/sparql/src/plan/types.rs`, ported from `sparopt`'s
`type_inference.rs` and mapped onto HornDB's kind-tagged dictionary `TermId`s): a small
bitset per variable, propagated bottom-up (`Join` intersects, `LeftJoin` marks the right
side `undef`, `Union` unions-with-undef). "Bound" is derived. One shared, sound-by-
construction pass feeds filter-pushdown legality, join-key discovery, and the
WCOJ-vs-hash decision. **Smart constructors** on `LogicalPlan` fold
empty/identity/constant-filter cases at build time.

### 5.2 Pass registry

A typed, ordered, individually-toggleable pipeline (`crates/sparql/src/plan/pass.rs`),
modeled on DuckDB's `RunOptimizer`/`OptimizerType` and ClickHouse's `IQueryTreePass`:

```rust
enum PassId { CoalesceBgp, Normalize, FilterPullup, FilterPushdown,
              ProjectionPushdown, JoinPlanning, /* … */ }

trait LogicalPass {
    fn id(&self) -> PassId;
    fn run(&self, plan: LogicalPlan, ctx: &PlanCtx) -> LogicalPlan;
}
```

- A driver runs passes in a fixed source order; each pass is skippable via a
  `disabled_passes` set (config + a query pragma), for harness bisection.
- **Debug-build validation after every pass** (lattice re-inference must still succeed;
  no dangling variables) — ClickHouse's `ValidationChecker` discipline.
- **Ordering constraints are declared, not commented.** Each `PassId` names the passes it
  must follow; the driver asserts the wired order satisfies them at startup (rejecting
  ClickHouse's "must run before X" rot).

Initial pass set (heuristic, always-beneficial — ship before any cost model):
`CoalesceBgp` → `Normalize` (`Equal→SameTerm`, constant folding) → `FilterPullup` →
`FilterPushdown` (predicate pushdown into pattern scans; legality gated on the lattice,
with the `LeftJoin`/`Minus` asymmetry) → `ProjectionPushdown` (bind only needed variables
— high value in a columnar dictionary store; overlaps existing `plan/pushdown.rs`) →
`JoinPlanning` (§5.5).

### 5.3 Statistics provider seam

A read-only trait the optimizer consults, implemented over SPEC-02 storage (and stubbed
otherwise). NDV + counts only in v1 (DuckDB's minimal set), sketches later behind the
same trait:

```rust
trait Stats {
    fn total_triples(&self) -> u64;
    fn predicate_count(&self, p: TermId) -> u64;          // |{ t : t.p == p }|
    fn ndv(&self, p: TermId, pos: Position) -> u64;       // distinct S or O for predicate p
    // later phases, same seam: quantile/min-max (range preds), count-min (=const selectivity)
}
```

RDF is *better* positioned than SQL here: the predicate is usually bound, so per-predicate
counts and per-position NDV are cheap and accurate straight from the columnar partitions.
Distinct counts are maintainable as HLL over dictionary IDs. **Who populates these under
the SPEC-06 delta model is a dependency, not designed here** — but the seam is defined so
`Cardinality`/`UniformEstimator` become one `Stats` impl (the zero-stats fallback) among
several. The ClickHouse lesson stands: a stats feature nobody maintains is dead weight —
gate the stats-backed estimator on it actually being populated, keep the fallback default
until it wins on the harness.

### 5.4 Cardinality estimator

Replace `_est`-ignoring with a real estimator over the `Stats` seam. Per-pattern base
cardinality from `predicate_count`/`ndv` (falling back to the `sparopt` static shape
table when a pattern's predicate is unbound). Join output via DuckDB's denominator model
— `∏ base / denominator`, denominator per shared variable ≈ `max(ndv)` over the patterns
binding it — **with**:

- **transitive-equality-class tracking** (a variable shared across ≥3 patterns — the RDF
  star/chain norm — is divided once, not per pair), and
- the **PK/FK-style cap**: an `owl:sameAs` / functional-property / key join never exceeds
  the smaller input. RDF makes this essential (sameAs closures otherwise explode).

Estimates memoized by variable/pattern bitset (DuckDB's `relation_set_2_cardinality`),
and surfaced through the existing `EXPLAIN` `~N rows` rendering.

### 5.5 Ordering — the one cost-based stage (bimodal)

`JoinPlanning` is the only pass that searches. It produces a `JoinSpec` per BGP and
chooses the physical shape, costed on one additive, cardinality-dominated scale
(`cost = card(result) + cost(children)`), extended to be **hybrid-aware**:

- **WCOJ sub-plan** → the plan object is a **variable elimination order**, not a binary
  tree. Ordering heuristic: most-constrained-variable / smallest-NDV-first (retargeting
  `sparopt`'s greedy connected seed-and-grow from "pick next pattern" to "pick next
  variable"), seeded by the current descending-degree tie-break. Cost estimated by an
  **AGM / fractional-edge-cover bound**, *not* a product of pairwise selectivities.
- **Binary-hash sub-plan** (the `≤ cutover` / low-arity case, and non-BGP algebra joins)
  → DuckDB-style: DP over the connected query (sub)graph for small relation counts,
  **greedy operator ordering past an explicit threshold + work budget**. Hash build-side
  chosen in a *late* pass (DuckDB's `BuildProbeSideOptimizer`), keeping the ordering
  search state small.
- **The WCOJ-vs-hash choice becomes part of costing**, replacing the fixed
  `wcoj_cutover == 4` — on unified memory the trie-materialization vs hash-table
  trade-off is memory-bandwidth-bound, so the cost model must carry a materialization
  term, not assume DuckDB's CPU-bound pairwise model.

This stage is where `Planner::choose` and `ExecutionPlan::for_bgp` in `horndb-wcoj` grow
their real bodies; the SPARQL `JoinPlanning` pass calls into that WCOJ planner for the
per-BGP `JoinSpec`, keeping the crate dependency direction (`sparql` → `wcoj`) intact.

### 5.6 Runtime filters (sideways information passing) — later phase

ClickHouse's `tryAddJoinRuntimeFilter` and any-join→semi/anti map cleanly onto RDF star
joins and `FILTER EXISTS`: build a set/bloom from one pattern's bindings, push it as a
skip filter into another pattern's scan. Natural fit for WCOJ intermediate bindings.
Deferred behind the pass registry (a later `PassId`), listed here so the framework
reserves the seam.

### 5.7 Where `PlanAdvisor` (ML) plugs in

Unchanged contract (SPEC-08 F2): `JoinPlanning` may construct a `SubplanShape` and call
`registry.plan_advisor().advise(...)`, treating the result as a hint validated against the
`Stats`-backed estimate and discarded past tolerance or the 1 ms p99 budget. The framework
gives ML a real symbolic baseline to advise *against*, which today's stub cannot.

### 5.8 Reasoning in the IR — the unifying bet (later phases)

HornDB today **materializes the full OWL 2 RL closure up front** (the canonical,
PTIME-tractable way to serve OWL 2 RL) and runs SPARQL as pure pattern matching
over the closed graph. Reasoning-strategy selection — compiled OWL-RL rule vs.
GraphBLAS closure resolver (SPEC-05 `delegate = "closure"`), SSSOM crosswalk
expansion, SKOS hierarchy resolution — is fixed at rule-compile / materialization
time, *upstream* of this optimizer (see `docs/architecture.md` §15). That leaves
joint optimizations on the table. Once demand-driven backward-chaining exists, a
query can answer without full materialization, and the engine faces a real
per-subgoal choice: **materialize vs. rewrite vs. delegate-to-resolver**.

The unifying claim of this spec is that this choice belongs in the *same* IR as
join ordering, because **reasoning is query rewriting**: applying a subclass rule
rewrites `?x a :C` into a UNION over subclasses; a transitive rule rewrites a
pattern into a recursive/fixpoint one. So reasoning enters the §5.1 logical IR as
**first-class rewrite passes + delegate nodes** — *not* as generic recursive
patterns a cost model grinds on:

- **Rewrite passes** (new `PassId`s in the §5.2 registry, running before
  `JoinPlanning`) expand/substitute patterns from the TBox; the optimizer then
  pushes filters through the expansion, orders joins across base + inferred
  patterns, and materializes only the closure slice the query reaches.
- **Delegate nodes** — heavy transitive closure hands off to the specialized
  GraphBLAS operator (SPEC-05) via a `ClosureScan` / the existing `PathClosure`
  (`Algebra::l`) node that the optimizer *chooses* but does not try to out-plan
  with join reordering. This is the hybrid (ADR-0005): materialize/delegate the
  closure subset, rewrite/backward-chain the rest.
- **Reasoning/materialization catalog seam**, parallel to §5.3's `Stats`: what is
  already closed + the cost of each resolver, so materialize-vs-rewrite-vs-delegate
  is cost-based on the §5.5 additive scale.
- **Machinery:** magic-sets / demand transformation (SPEC-03 F4) + SLG tabling
  (F5) generate the query-driven rewrites; SPARQL backward-chained entailment mode
  (SPEC-07) is the surface.

**Prior-art blind spot:** the three systems surveyed above (Oxigraph `sparopt`,
DuckDB, ClickHouse) are all *non-reasoning* engines, so none of them informs this
layer — it is HornDB-specific and has no borrow-from-X answer. The hard open
problem is cost/cardinality/termination for recursive fixpoints in a model that
otherwise assumes non-recursive AGM/hash costing (§8).

## 6. Phasing

Each phase is independently shippable and harness-gated; the framework (5.1–5.2) lands
first so everything else has a home.

1. **Framework scaffolding.** Logical IR + flat BGP coalescing + binding/type lattice +
   smart constructors + pass registry with validation. Port existing heuristic rewrites
   (`plan/pushdown.rs`) onto it. **No behavior change** — golden-plan tests prove the
   pipeline reproduces today's plans.
2. **Heuristic rewrite passes.** Filter pull-up/push-down, projection pushdown,
   `Equal→SameTerm`. Always-beneficial; no statistics. Guarded by the slot-differential
   suite + conformance harness.
3. **Statistics seam + NDV/counts estimator.** `Stats` trait over SPEC-02, real
   `Cardinality`, memoization, `EXPLAIN` wired to it. `UniformEstimator` demoted to
   fallback. (Depends on SPEC-02 exposing counts/NDV — coordinate with SPEC-06 for
   maintenance under deltas.)
4. **Cost-based `JoinPlanning`.** Bimodal ordering (WCOJ variable-order + AGM bound;
   hash DP/greedy), cost-driven WCOJ-vs-hash, late build-side pass. Retires
   `wcoj_cutover == 4` as a hard rule.
5. **Later (optimizer):** sketches (quantile/count-min) behind `Stats`; runtime
   filters (§5.6); ML `PlanAdvisor` validation loop (§5.7).
6. **Reasoning in the IR (§5.8).** Reasoning-as-rewrite passes + the
   reasoning/materialization catalog seam; cost-based
   materialize-vs-rewrite-vs-delegate; property-path closure routed through the
   SPEC-05 GraphBLAS backend by selectivity (SPEC-07 F3 fast path).
7. **Backward-chaining.** Magic-sets / demand transformation (SPEC-03 F4) + SLG
   tabling (F5); SPARQL backward-chained entailment mode (SPEC-07). This is where
   the "hybrid forward/backward" core bet (ADR-0005) becomes real.

## 7. Acceptance criteria

1. **No-regression baseline (phase 1).** The pass pipeline reproduces every current plan;
   the SPARQL conformance subset (`harness/selected.toml`) and the WCOJ differential
   fuzzer stay green. Golden-plan snapshots exist for a representative query set.
2. **Pass legibility.** Every pass is individually disable-able via config/pragma; the
   driver asserts declared ordering constraints at startup; debug builds validate the IR
   after each pass. A regression can be bisected to a single `PassId`.
3. **Estimator accuracy (phase 3).** On the conformance subset, `EXPLAIN` cardinality
   estimates are within an order of magnitude of measured row counts on ≥ X% of nodes
   (threshold TBD from a baseline run) — strictly better than `UniformEstimator`.
4. **Ordering win (phase 4).** On the SPEC-03 acceptance shapes (the 4-cycle and the
   WatDiv/LUBM subset) the cost-based planner matches or beats the descending-degree /
   fixed-cutover heuristic on the harness, with **zero** result-set changes vs the WCOJ
   differential oracle. No query regresses beyond a set tolerance.
5. **Cutover replacement.** At least one BGP shape exists where the cost model correctly
   picks a plan the fixed `wcoj_cutover == 4` rule got wrong (documented in the harness).
6. **ML neutrality preserved.** With `ml.enabled = false`, plans are bit-identical to a
   no-ML build (SPEC-08 F2), and the advisor path respects the 1 ms p99 skip budget.

## 8. Open questions / uncertainties

- **SPEC-02 statistics ownership.** This spec defines the `Stats` seam but not its
  maintenance. Does SPEC-02 grow per-predicate counts/NDV as first-class, and does SPEC-06
  update them incrementally under deltas, or is there a periodic recompute? Blocks phase 3.
- **AGM cost calibration.** The fractional-edge-cover bound gives an upper bound, not an
  expected size; how loose is it in practice on HornDB workloads, and does it need a
  learned/empirical correction to be comparable with the hash-tree cost on one scale?
- **Where the WCOJ planner ends and the SPARQL planner begins.** `JoinSpec` production
  for a BGP lives in `horndb-wcoj`; the surrounding algebra ordering lives in
  `horndb-sparql`. The exact API between them (does `wcoj` see the `Stats` seam directly,
  or only a digested per-pattern estimate?) needs pinning in the implementation plan.
- **Recursive-fixpoint costing (§5.8).** The optimizer's cost model assumes
  non-recursive AGM/hash costing; reasoning rewrites introduce recursion
  (transitive closure, rule fixpoints). How do cost, cardinality, and termination
  for a fixpoint node fit the same additive scale — and how much must simply be
  delegated (opaque) to the closure operator rather than costed? Blocks phases 6–7.
- **Verify-before-cite version facts.** ClickHouse version/figure claims (25.9 join
  reordering, ~26.4 auto-stats, TPC-H speedups) and DuckDB's exact greedy threshold are
  search-snippet-level in the source briefs — confirm against primary docs before any of
  them appear in user-facing material or a benchmark writeup.

## Sources

Per-system engineering briefs (commissioned 2026-07-06) with exact source URLs are
preserved in the git history of this change / the epic issue #TODO. Primary sources read:
Oxigraph `lib/sparopt/src/{optimizer,algebra,type_inference,lib}.rs`; DuckDB
`src/optimizer/{optimizer.cpp,join_order/*}`; ClickHouse
`src/Analyzer/QueryTreePassManager.cpp` and `src/Processors/QueryPlan/Optimizations/*`.
Academic lineage: Veldhuizen (Leapfrog Triejoin, ICDT'14); Moerkotte & Neumann (DPccp
VLDB'06, DPhyp SIGMOD'08); Leis et al. ("How Good Are Query Optimizers, Really?" VLDB'15);
Ebergen ("Join Order Optimization with (Almost) No Statistics", DuckDB MSc thesis).
