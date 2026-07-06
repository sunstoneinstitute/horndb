# Count-pushdown extensions — filter-aware, grouped, multi-count (design)

**Date:** 2026-07-06
**Issue:** [#128](https://github.com/sunstoneinstitute/horndb/issues/128) (SPARQL aggregation runtime; remaining-work item 2: "filter-aware / grouped / multi-aggregate count pushdown").
**Builds on:** `docs/specs/2026-06-30-streaming-runtime-pushdown-design.md` §3(b) — the landed `CountScan` + `Executor::count_bgp` first cut.
**Status:** design — implementation plan at `docs/plans/2026-07-06-count-pushdown-extensions.md`.

## Problem

The landed aggregate pushdown covers exactly one shape: `COUNT(*)`/`COUNT(?v)`
over a *bare* `BgpScan` — no GROUP BY key, no DISTINCT, no intervening Filter
(`crates/sparql/src/plan/pushdown.rs::push_aggregates`). Everything else still
materializes one `Row(Vec<Slot>)` per BGP solution through `scan_bgp_ids`, then
buffers member rows per group in `GroupOp`/`eval_group_native` — even when the
aggregates only need *counts*. In the `agg_profile` battery
(`crates/sparql/examples/agg_profile.rs`), Q1 is pushed down but Q2
(`GROUP BY ?cat COUNT(?s)`) and Q3 (2-pattern BGP + `GROUP BY`) still pay the
full per-row materialization tax; the SPB mix contains the same shapes.

This increment extends the pushdown pass to more count-only shapes. The pass
stays **cost-model-free**: every rewrite below is heuristic-safe (never worse
than the plan it replaces) and applied unconditionally.

## Decision summary

| # | Shape | Decision |
|---|---|---|
| 1 | `COUNT` over `Filter(BgpScan)` where the filter is a conjunction of `?v = <const>` / `sameTerm(?v, <const>)` | **Cover** — inline the constants into the triple patterns, drop the Filter, then count-lower as usual |
| 2 | `GROUP BY ?k…` + plain `COUNT`(s) over a bare (or equality-filtered) `BgpScan` | **Cover** — new `GroupCountScan` plan node + additive `Executor::count_bgp_grouped` seam |
| 3 | Multiple aggregates, **all** plain counts (`COUNT(*)` / `COUNT(?bound-bgp-var)`, non-DISTINCT) | **Cover** — same `GroupCountScan` node; every output var carries the group size |
| 4 | Mixed aggregates (`COUNT` + `SUM`/`AVG`/`MIN`/`MAX`/`SAMPLE`/`GROUP_CONCAT`) | **Defer** — value aggregates need member values; no always-beneficial partial pushdown exists (see below) |
| 5 | `COUNT(DISTINCT …)`, grouped or not | **Defer** — needs a distinct-count seam; feasible later via TermId-hashing (noted as future work, #TODO) |
| 6 | Non-equality filters (ranges, `REGEX`, …) under a count | **Defer** — not expressible as pattern constants; pushing expression eval below the seam is out of scope |
| 7 | Partial filter inlining (inline the equality conjuncts, keep a residual Filter) | **Defer** — keeps the Group anyway (no count node), and turning it on for general plans is general constant-folding, not count pushdown |
| 8 | Zero-aggregate `GROUP BY` (`SELECT ?k … GROUP BY ?k`) | **Defer** — would work as a distinct-keys pushdown on the same node, but adds test surface for a shape absent from the SPB mix |

## Covered shape 1 — equality-filter inlining

**Recognized:** `Group { keys, aggregates }` over
`Filter { expr } over BgpScan { patterns }` where

* `expr` decomposes as an `Expr::And` tree whose every leaf is
  `Expr::Eq(?v, const)` or `Expr::Eq(const, ?v)` with `const ∈ {Term::Iri,
  Term::Literal}` (never `BlankNode`/`Var`/`Triple`);
* each equality variable appears in **exactly one** conjunct, is an output
  variable of the (pre-substitution) BGP, and is **not** a GROUP BY key;
* the surrounding `Group` qualifies as a count-only shape (covered shapes 2/3,
  or the landed single-`CountScan` shape). The inlining never fires on its own.

**Rewrite:** substitute each constant for its variable in every position of
every pattern (recursing into RDF 1.2 `Term::Triple` sub-patterns), drop the
`Filter`, then apply the count lowering to the now-bare `BgpScan`.

**Why this is result-identical (in this engine).** Three facts line up:

1. `Expr::Eq` evaluates as **structural `Term` equality**
   (`runtime.rs::eval_expr`, the `Expr::Eq` arm) — not numeric value equality
   (only `Expr::In` adds `compare_terms` value comparison). spargebra's
   `SameTerm` also lowers to `Expr::Eq` (`algebra/translate.rs`), so
   `FILTER sameTerm(?v, <c>)` is covered for free.
2. Query-side constants and decoded data-side terms are **normalized
   identically**: both are printed by oxrdf (`translate.rs` uses
   `Literal::to_string`; `horn.rs::oxrdf_to_algebra` likewise), so xsd:string
   collapsing and language-tag lowercasing agree on both sides of the `==`.
3. BGP constant matching resolves the constant through the same oxrdf form to
   a dictionary id (`horn.rs`: `dict.get(&algebra_to_oxrdf(c)?)`), and
   dictionary ids *are* term identity. A constant absent from the dictionary
   yields an empty scan / zero count — exactly what a filter passing no rows
   yields.

Since a bare BGP binds every one of its output variables in every solution,
`FILTER(?v = c)` keeps exactly the rows whose `?v`-term equals `c`'s term,
which is exactly the solution set of the BGP with `c` substituted for `?v`.
A repeated variable *within one pattern* (`?v <p> ?v`) substitutes to a ground
or partially-ground pattern with the same semantics as the diagonal filter.

**Guards worth spelling out:**

* `?z = <c>` with `?z` **not** produced by the BGP filters out *every* row
  (engine Eq: unbound ⇒ `None == Some(c)` ⇒ false), while substitution would
  be a no-op — hence the membership guard.
* The same variable in two conjuncts (`?v = <a> && ?v = <b>`) may be
  unsatisfiable; we bail rather than reason about constant-vs-constant.
* A substituted variable disappears from the scan's output columns, which is
  only safe because count-only aggregates never read member values and
  `COUNT(?v)` of an equality-constrained `?v` still counts every surviving row
  (the bound-var check runs against the **pre-substitution** BGP vars). This
  is why the inlining is scoped to count shapes instead of being a general
  constant-folding pass (which would need a compensating `Extend` to keep the
  variable visible downstream — deferred).

**Coupling note (recorded on purpose):** W3C SPARQL `=` on numeric literals is
*value* equality (`"01"^^xsd:integer = "1"^^xsd:integer`). Stage-1 `Expr::Eq`
is term equality — a documented engine-wide approximation. If `Eq` ever gains
numeric value semantics, the **literal**-constant case of this rewrite must be
restricted to IRIs. A parity test pins the current agreement using
value-equal-but-term-distinct literals (`"42"` vs `"042"`): both paths count
only the exact term.

## Covered shapes 2 + 3 — grouped / multi-count pushdown

**Recognized:** `Group { keys, aggregates }` over a bare `BgpScan` (possibly
after shape-1 inlining) where

* `aggregates` is non-empty and **every** aggregate is non-DISTINCT and either
  `CountStar` or `Count(?v)` with `?v` an output variable of the
  (pre-substitution) BGP — so each aggregate's value is the group size;
* every key is an output variable of the BGP (a key the BGP does not bind
  would group everything under `Unbound`; the streaming `Group` handles that
  rare shape, we bail);
* not the already-landed single case (`keys == [] && aggregates.len() == 1`
  stays `CountScan` — no churn to landed code).

**Rewrite target — new physical node:**

```rust
/// Pushed-down grouped/multi COUNT over a BGP: one row per group carrying the
/// key slots and, per output var, the group's solution count. Every aggregate
/// this node replaces is a plain (non-DISTINCT) count of the group size.
GroupCountScan {
    patterns: Vec<TriplePattern>,
    keys: Vec<Var>,          // possibly empty (implicit group, ≥2 counts)
    out_vars: Vec<Var>,      // one per replaced aggregate, in order
}
```

Output schema = `keys ++ out_vars`, matching `group_output_schema` exactly.

**New additive seam** (default keeps every non-Horn backend correct):

```rust
/// Per-group solution counts for a BGP grouped by `keys`. `None` = "no fast
/// grouped count" (caller falls back to scan + hash-count). When `Some`, the
/// groups MUST partition the rows `scan_bgp_ids` would produce, keyed by
/// term identity of the key columns.
fn count_bgp_grouped(
    &self,
    _patterns: &[TriplePattern],
    _keys: &[Var],
) -> Result<Option<Vec<(Vec<Slot>, usize)>>> {
    Ok(None)
}
```

`HornBackend` implements it with the same verbatim pattern-compilation the
other three seam methods share, then streams the WCOJ Arrow batches and hashes
the raw `u64` key columns (`HashMap<Vec<u64>, usize>`) — **no `Row` is ever
built, no term is decoded**. Checked during design: `horndb-wcoj` exposes no
per-key cardinality (its `cardinality.rs` is a uniform-estimator stub), so
batch-level key hashing at the seam is the cheapest honest implementation.
Fallback-to-`None` cases mirror `count_bgp`: a within-pattern repeated
variable (diagonal filter), or a key with no WCOJ column. A constant missing
from the dictionary or a failed ground-pattern membership test returns
`Some(vec![])` (zero groups), matching the empty scan.

Column pruning alone does *not* capture this win: even pruned to the key
column, `scan_bgp_ids` allocates a `Row(Vec<Slot>)` per solution, `ProjectOp`
remaps every row, and `GroupOp` buffers member rows per group. The seam skips
all per-row allocation, the same lever that made `count_bgp` collapse the
269 ms `COUNT(*)`.

**Operator.** `GroupCountScanOp` (in `exec/op/source.rs`, next to
`CountScanOp`):

* `keys.is_empty()` (implicit group, ≥2 counts): reuse `count_bgp` (fast or
  scan+len fallback) and emit **one row** — counts are `0` on an empty input,
  matching SPARQL §11.2 / `eval_group_native`'s implicit empty group.
* keys non-empty: `count_bgp_grouped`, falling back to
  `scan_bgp_ids` + `KeyPart` hash-counting on the key columns only (a
  members-free re-statement of `eval_group_native`'s grouping). Zero solutions
  ⇒ zero output rows (no implicit group when keys exist) — same as `Group`.
* Counts are emitted as `Slot::Term(integer_literal(n))`, key slots keep their
  scan provenance (`Slot::Id` on Horn, `Slot::Term` on the fallback), exactly
  like the streaming path.

**Ordering parity is mandatory, not cosmetic.** `eval_group_native` sorts its
output by the decoded-lexical form of the key slots, and that order is
observable whenever a `Slice` (LIMIT) sits above the Group. `GroupCountScanOp`
therefore sorts by the identical `Vec<Option<String>>` key (decode each key
slot, `lex(...)`, `Unbound → None`) before emitting. Sorting decodes
`O(|groups| × |keys|)` terms — negligible next to the avoided per-row work.
(Two distinct groups with equal lexical keys tie non-deterministically in the
streaming path today — a pre-existing property; parity tests use distinct-lex
keys.)

## Deferred, with reasons

* **Mixed count + value aggregates (4).** `SUM`/`AVG`/`MIN`/`MAX`/`SAMPLE`/
  `GROUP_CONCAT` need member *values*, so full pushdown is impossible. The
  partial option — split the Group into a `GroupCountScan` joined with a
  residual value-`Group` — adds a hash join the current plan doesn't have;
  whether that wins depends on group count vs row count, i.e. it needs the
  cost model this pass deliberately does not have. Column pruning already
  narrows the value-Group's input to `keys ∪ agg-input vars`. Defer.
* **`COUNT(DISTINCT ?v)` (5).** Answerable without materializing rows by
  hashing `(key ids…, value id)` pairs at the seam — TermIds are term
  identity, so distinct ids = distinct terms. That is a *new* seam contract
  (distinct-count) with its own parity obligations; `agg_profile` Q5 is the
  motivating shape. Deliberately its own future increment (#TODO — file when
  picked up).
* **Non-equality filters under counts (6)** — a range like `?o > "20"` cannot
  become a pattern constant; a `count_bgp_filtered` seam would push expression
  evaluation below the executor seam, inverting the layering the 2026-06-30
  design fixed. Defer indefinitely; revisit only with seam-level statistics.
* **Partial inlining / general constant folding (7).** Inlining only *some*
  conjuncts leaves the Group + residual Filter in place (no count node), so
  the win is a narrower scan on a shape we cannot verify is hot; and folding
  outside count shapes must preserve the substituted variable via an added
  `Extend`. Both are general-planner work, not count pushdown.
* **Zero-aggregate GROUP BY (8)** — `GroupCountScan { out_vars: [] }` would
  compute distinct keys; correct but untested surface for a shape not in the
  SPB mix. Revisit if it shows up in a profile.

## Parity guarantee and test plan

For every covered shape: **pushed-down result == fallback streaming result on
the same data**, including row order (LIMIT-observable), enforced by tests in
`crates/sparql/src/plan/pushdown.rs`'s existing battery style
(`Runtime::run` vs `run_unpruned_for_test`, `canon` for multiset equality,
direct `Vec` equality where order matters):

1. Structural: eq-filtered counts rewrite to `CountScan`; grouped/multi counts
   rewrite to `GroupCountScan`; every guard violation (unbound filter var,
   repeated conjunct var, non-equality op, DISTINCT aggregate, mixed
   `COUNT`+`SUM`, key-substituting filter, key ∉ BGP vars, `COUNT(?z)` of an
   unproduced var) stays a `Group` **and** stays result-correct.
2. Value parity on `HornBackend` for the whole new query battery, plus the
   term-identity pin (`"42"` vs `"042"`) and a `sameTerm` case.
3. Ordering parity: grouped count under `LIMIT` compares full `Vec<Bindings>`
   equality (not canonicalized) between the two paths.
4. Seam parity in `horn.rs` tests: `count_bgp_grouped` == grouping of
   `scan_bgp_ids` output; diagonal-repeat and unbound-key cases return
   `Ok(None)`.
5. Fallback correctness on `MemStore` (default seam ⇒ `None`) via a hand-built
   `GroupCountScan` plan, mirroring `count_scan_falls_back_when_count_bgp_is_none`.
6. Existing gates stay green: the `slot_differential` proptest, the pushdown
   `rewrite_is_result_invariant` battery, `cargo nextest run -p horndb-sparql`
   (and `--features server`).

Two landed scope-guard cases flip from negative to positive and move
accordingly: "GROUP BY: non-empty keys" and "Two aggregates" (both in
`scope_guard_keeps_group_and_stays_correct`) now push down.

## Non-goals

* No cost model, no statistics — every rewrite fires unconditionally because
  it is never worse than what it replaces.
* No change to `scan_bgp_ids`, `count_bgp`, or any landed operator; both new
  pieces (seam method, plan node) are additive.
* No metrics surface changes (`crates/metrics/` untouched).
* Benchmarks: `agg_profile` locally is a smoke check only; official
  aggregation-qps moves are measured by the SPB-256 nightly on hornbench and
  recorded in `BENCHMARKS.md` by the executing session (per root `CLAUDE.md`).
