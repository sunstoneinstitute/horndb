# SPEC-07 EXPLAIN pragma (#53)

Increment of the SPEC-07 SPARQL epic (#7). Implements the non-standard
`EXPLAIN` pragma (SPEC-07 F9, acceptance #5): return the chosen physical
plan with execution mode and per-node cardinality estimates instead of
executing the query.

## Scope (this increment)

- Recognise a leading `EXPLAIN` pragma (case-insensitive), optionally
  `EXPLAIN JSON`, in the parser. The pragma is stripped before the
  remaining text is handed to spargebra (which does not know it).
- A `PhysicalPlan` pretty-printer (`plan::explain`) with per-node
  cardinality annotations and a header line carrying the execution mode.
- A best-effort cardinality estimate exposed through the `Executor`
  trait (`cardinality_estimate`, default `None`); `MemStore` and
  `HornBackend` provide real counts.
- Surface via `api` (`QueryAnswer::Explanation`) and the `/query` HTTP
  handler (text or JSON plan, by pragma).

## Execution mode honesty

Backward-chained mode (#55) is not yet implemented. The only modes that
exist are the entailment-regime markers (`simple` /
materialized OWL-RL). EXPLAIN therefore reports the **materialized**
execution mode for every query today, with a note that backward-chaining
is not yet wired. When #55 lands, the explain header gains the real mode
selection. This is the honest current state; acceptance #5's "materialized
vs backward" wording is satisfied insofar as the mode is shown and labelled.

## Cardinality model

Stage-1 estimates are deliberately simple — there is no cost model
(`plan::planner` is a 1:1 lowering). Each `BgpScan` leaf is estimated by
asking the executor for a count of matching triples (exact for `MemStore`,
which holds a `HashSet`; a bounded scan for `HornBackend`). Composite
nodes combine child estimates with textbook rules (join = product bounded
by the larger side as an upper bound; union = sum; filter/distinct/slice
shrink; project/extend/order pass through). The numbers are estimates, not
guarantees, and are labelled `~`.

## Out of scope (deferred)

- Real cost-based planning / index selection (the spec mentions "chosen
  indexes"; there is no index chooser yet — the physical plan is a 1:1
  lowering, so the printer reports the BGP scan strategy, not an index
  pick). Deferred until a cost model exists.
- Backward-chained mode display (#55).

## Tests

- Parser: `EXPLAIN SELECT …`, `EXPLAIN JSON SELECT …`, case-insensitive,
  leading whitespace/comments; non-EXPLAIN queries unaffected.
- Plan printer: a recursive Kleene path (`subClassOf+`, acceptance #5)
  shows the closure node, the mode line, and cardinality annotations.
- API: `execute_query` on an EXPLAIN query returns `Explanation`, never
  runs the query.
- Server: `/query` with an EXPLAIN body returns the plan text/JSON.
