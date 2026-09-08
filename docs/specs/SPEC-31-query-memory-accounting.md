---
status: draft
date: 2026-09-08
scope: "SPEC-31 — what `max_query_memory` bounds: per-query charging of the executor's blocking-operator row buffers, the refusal contract when a query crosses its ceiling, the default, what each later phase adds to the accounting, and why store-side index growth is bounded elsewhere"
---

# SPEC-31 — Per-query memory accounting

**One-line thesis:** A single SPARQL query can allocate until the machine dies,
and on 2026-09-05 one did — an LDBC SPB run exhausted a 124 GiB benchmark host
and took it off the network for a day. This spec makes `max_query_memory` mean
something: the executor charges the buffers it accumulates, and a query that
would cross its ceiling is refused instead of served.

**Companion to SPEC-26.** SPEC-26 S5 defines the `max_query_memory` *knob* —
parsed, layered, per-query overridable — and names enforcement a non-goal,
"delegated to a companion spec written when that work is picked up". This is
that spec. SPEC-26 needs no amendment: it anticipated exactly this.

**Refines:** the SPEC-07 executor (`crates/sparql/src/exec/`). It does not
change SPARQL semantics. A query that fits its budget behaves exactly as
before; a query that does not now fails instead of running the host out of
memory.

## Problem

`[server.limits].max_query_memory` was parsed, carried on `QuerySettings` and
accepted as a per-query URL override — and bounded nothing. The only backstop
on a query's footprint was `max_concurrent_queries`, which limits *how many*
queries run, not what any one of them may allocate.

What that costs, measured (HDB-167, hornbench, 234 M-triple LDBC SPB corpus):

| | |
|---|---|
| server at rest, corpus loaded | 23.7 GiB |
| server during one query from the SPB driver's parameter-sampling phase | **90 GiB**, in under two minutes |
| that query, isolated | `SELECT (COUNT(?cw)) { ?cw a cwork:CreativeWork }` — 4.89 M matches, 88 s, **+40.6 GiB** |
| the same query with `HORNDB_DIRECT_SOURCE=1` (no memoised source) | 67 s, **+16.6 GiB** |
| outcome without a ceiling (SF=0.256, 2026-09-05) | host exhausted, ~100 min of kernel memory pressure, journald killed by its watchdog, runner offline for a day |
| outcome with a 90 GiB cgroup ceiling (SF=0.128, 2026-09-07) | server killed by the kernel inside its cgroup, **host unaffected** |

The cgroup ceiling is a host guard, not a fix: it converts "the machine dies"
into "the server dies". Neither is an acceptable answer to one expensive query.
The server should refuse the query and keep serving.

**Neither number in the A/B was query memory.** HDB-229 measured where the
growth went. The ~24 GiB is the memoised whole-scope `VecTripleSource` that
the first query on a commit version builds and every later query reuses. The
~16.6 GiB was a second index: the query binds predicate and object, so the
trie reads an object-major ordering, and building one laid out the `(o, s)`
columns of every predicate partition in the graph. Both are store-side — built
once, kept for the life of the process, shared by later queries. The executor
holds none of it. HDB-229 removed the second index for this shape; HDB-230
tracks what remains of the first. See "What this spec does not bound, and
why" below.

A corollary worth recording: a serving footprint measured at load time
understates the real one badly. The SF=0.128 corpus loads in 23.7 GiB and
settles at ~64 GiB once queried. At SF=0.256 the same ratio puts the served
footprint past the 124 GiB host, which is why 2026-09-05 never had a chance.

## Non-goals

- **Allocator-level accounting.** A `GlobalAlloc` shim attributing every
  allocation to the query on whose thread it happened would be exact and would
  also tax every allocation in the process. This spec charges at the executor's
  accumulation points instead, which is where unbounded growth actually lives.
- **Bounding the store, dictionary or index memory.** Those are sized by the
  corpus, not by a query, and belong to SPEC-02/SPEC-25. This includes
  index-like structures a query *triggers* but does not own: the snapshot
  memo, derived orderings, and direct-source leaves. They are amortised
  across queries, so a per-query budget is the wrong instrument for them (see
  below). They get a separate bound, HDB-231.
- **Spilling to disk.** An over-budget query is refused, not spilled. External
  sort/hash is a later phase and a much larger change (it makes blocking
  operators restartable); the bound has to exist before spilling has anything
  to trigger on.
- **A server-wide memory budget across concurrent queries.** The bound is
  per-query. The worst case is therefore `max_query_memory ×
  max_concurrent_queries`, which an operator sizes deliberately. A shared pool
  with per-query fair-share is phase 3.

## Requirements

### S1. Charge the executor's blocking-operator buffers

Every operator that must hold its whole input before it can emit anything
charges those rows to the query's budget, and holds the charge for as long as
it holds the rows:

| operator | what it accumulates |
|---|---|
| `GroupOp` (GROUP BY, aggregates) | the entire child input |
| `OrderByOp` (ORDER BY) | the entire child input |
| `UnionOp` | both children, combined and normalized |
| `JoinOp`, `LeftJoinOp`, `MinusOp` | the build side (right), before probing |
| `PathClosureOp` (`p+` / `p*`) | the edge set |

These share one funnel — `exec::op::blocking::drain` — so the charge is levied
in one place for six of the seven. `UnionOp` accumulates inline (it must
normalize across both children) and charges the same way.

A **streaming** operator that keeps no state across chunks holds one chunk at
a time whatever the result size and so charges nothing. That is the bound
working as specified, not a gap, for such an operator: a query with only
those is already bounded by construction, and result size is
`max_result_rows`'s job.

`DistinctOp` is the exception, and it is a real gap, not a specified bound.
It pulls one chunk at a time like any streaming operator, but keeps a `seen`
set of every distinct row's key across the whole query — state that grows
with the number of distinct rows and is never charged. A `SELECT DISTINCT`
over a high-cardinality pattern can accumulate as much memory as a `GROUP
BY` over the same pattern, uncharged. Charging it is HDB-232, not this
phase — see "What the charge does and does not cover".

### S2. Refuse, never truncate

Crossing the ceiling fails the query with a typed error naming the limit and
what the charge would have totalled. Like SPEC-26 S5's `max_result_rows`, the
result is never silently shortened — a short answer must never be mistakable
for a complete one.

The charge is rejected atomically: a `grow` that would cross the line adds
nothing, so the budget after a refusal is exactly what it was before.

Over HTTP the refusal is **507 Insufficient Storage**, not 400. The query is
well-formed and would succeed at a larger budget; blaming the client for a
legal request is wrong, and 507 says what actually happened. (SPEC-26 S5 set
the precedent by giving `query_timeout` its own 504 rather than folding it into
400.)

### S3. Per-query scope, on the thread-local footing the cancel token uses

The budget is thread-local, for the reason `exec::cancel` is: the server runs
one query per blocking-pool thread and the operator tree is `!Send`, so a
thread-local *is* the per-query scope, and threading a budget parameter through
every operator constructor and `Executor` method would buy nothing.

The scope guard **resets** on drop rather than restoring: blocking-pool threads
are reused, and a finished query's charge left installed would bill the next
query to land on that thread.

### S4. A default that is a real number

`max_query_memory` defaults to **8 GiB**, not `None`.

`None` (unbounded) remains expressible, because an operator who has measured
their workload may want it. But it is not the default: the default is what runs
on a machine nobody has tuned, and unbounded is what cost a day of bench-host
downtime.

8 GiB is chosen to be generous for any query a well-sized deployment actually
runs, and small enough that `max_concurrent_queries` queries at the ceiling do
not exhaust a normal server. It is deliberately **not** derived from total RAM:
the config layer has no reliable view of what else shares the machine, and a
limit that silently grows with the host is one nobody can reason about.

### S5. Observability

Two series (`docs/metrics.md` carries the rows):

- `horndb_sparql_query_memory_peak_bytes` — histogram, the peak charge of one
  query, observed once per query on every exit path including a refusal. This
  is the series that answers "which queries accumulate, and how much".
- `horndb_sparql_queries_over_budget` — counter, queries refused on budget.

## What the charge does and does not cover

The budget bounds the executor's **growth**, and is not an accounting of the
process. A query always uses somewhat more than its charge says. Not counted:

- the store, dictionary and indexes (corpus-sized, not query-sized);
- WCOJ iterator state during a scan;
- the response serialization buffer (bounded separately by the stream channel);
- allocator overhead per allocation, and `Term::Triple`'s nested patterns,
  neither of which `Row::heap_bytes` walks — so the charge reads slightly low;
- the materialized result of the non-streaming path: `execute_query`'s
  `QueryAnswer::Solutions { rows }` and the server's `run_materialized`
  (ASK/CONSTRUCT/DESCRIBE/EXPLAIN) collect every row before serializing, and
  that collection is not charged. Streaming SELECT is bounded by
  `max_result_rows`; the materialized path is bounded by neither. Known gap,
  phase 2 candidate; found by reading, not measured.

The list below is a different class from the one above: **per-query**
memory, rows a query holds for its own duration and frees when it ends, the
same class S1 charges — simply not charged yet. It is not the store-side
growth in "the store, dictionary and indexes" bullet above, or in "What this
spec does not bound, and why" below, either of which lives past any one
query. Charging this per-query class is HDB-232:

- `DistinctOp`'s `seen` set (see S1) — grows with the number of distinct
  rows, held for the query's whole life;
- the BGP scan itself. `ScanOp` wraps a fully materialized `Batch`, and the
  default `scan_bgp_ids` accumulates every WCOJ output batch into one
  `Vec<Row>` before returning it — the largest per-query row buffer in the
  engine, and it exists even for a query with no blocking operator at all
  (see acceptance criterion 2);
- `UnionOp` charges each chunk before `normalize_columns` rewrites
  `Slot::Id` cells into decoded `Slot::Term` strings, so on a column mixing
  provenance across the two children the bytes it ends up retaining can run
  several times what it charged;
- the two operators where blocking output can exceed blocking input:
  `PathClosureOp` charges the edge set it drains but not the closure it
  computes from it (which can be quadratic in the edge count), and the hash
  joins (`JoinOp`, `LeftJoinOp`, `MinusOp`) charge the build side but not a
  probe chunk's matched-row fan-out held in `pending`;
- `fallback_group_counts`, the path `GroupCountScan` takes when the backend
  has no `count_bgp_grouped` fast path: it materializes a full scan batch
  and a grouping hash map, both uncharged and, unlike the store-side memo
  acceptance criterion 7 pins, invisible in `memory_split().snapshots` too —
  there is no instrument that shows this growth at all today.

Writing that down is the point: an operator setting 8 GiB should read it as "no
query's blocking operators may accumulate more than 8 GiB", not as "no query
may add more than 8 GiB of RSS".

## What this spec does not bound, and why

The 16.6 GiB in the Problem measurement was a second index, not query memory:
the demonstration query binds predicate and object, so the trie reads an
object-major ordering, and building one laid out the `(o, s)` columns of every
predicate partition in the graph. Two paths can produce it — a direct source
builds a leaf for every predicate; a memoised source derives one whole-scope
ordering from the `Pso` anchor and reuses it. Either way, the index is
store-side: built once, kept for the process, and shared by every later
query, including ones that never asked for it.

A per-query budget must not charge it, because the bound would depend on
arrival order. The first query to need the ordering would be refused at
8 GiB; an identical second query would be served from the index the first
was refused for building. A per-query limit has to be a property of the
query alone. Store-side growth needs a bound whose unit is the store — a
ceiling on the memo, or a decision not to build — not a charge to whichever
query arrives first.

That bound is tracked separately: HDB-230 (do not derive the second ordering
on the default path) and HDB-231 (the store-side ceiling). After HDB-229 the
demonstration query allocates almost nothing on either path, which is why the
original acceptance criterion 6 was replaced rather than kept open.

## Status — phase 1 bounds what it says it bounds

Measured on hornbench, 2026-09-08, commit `917f8f0`, SF=0.128 corpus, server
ceiling raised to 1 TiB so the query would complete and its charge be readable:

| | |
|---|---|
| `SELECT (COUNT(?cwUri)) { ?cwUri a cwork:CreativeWork }` | HTTP 200 in 96 s |
| server RSS | 23,750 → **64,426 MiB** |
| `horndb_sparql_query_memory_peak_bytes_sum` | **0** |
| the same query with `?max_query_memory=8GiB` | HTTP 200, `queries_over_budget_total` **0** |

**The zero was correct, not a gap.** The budget charged this query nothing
because the aggregate is served by the `CountBgp` pushdown (#144), which
yields one row and drains nothing — there is no accumulation for `drain` to
charge. The 40.7 GiB of RSS growth was in the store's snapshot memo and
object-major index, not in a blocking operator's buffer, and the instrument
said exactly that.

GROUP BY, ORDER BY, UNION, the hash-join build sides, MINUS and path closure
are charged and refuse over budget. Tasks 2–4 of this spec's implementation
plan add the tests that prove each acceptance criterion below, including the
rewritten criterion 6. That covers every blocking operator S1 lists —
what this phase set out to charge. It is not every uncharged per-query
accumulation in the executor: `DistinctOp`'s seen-set, the whole BGP scan,
and the under-charges in `UnionOp`, `PathClosureOp` and the hash joins are
real gaps, tracked as HDB-232 (see "What the charge does and does not
cover").

The cgroup ceiling (`MEMORY_MAX` in `crates/harness/scripts/start-engine.sh`)
remains the host guard against *store-side* growth until HDB-231 lands.

## Phases

- **Phase 1 (this spec, landed).** Charge blocking-operator row buffers; refuse
  over budget; 8 GiB default; the two metrics. HDB-229 (merged) removed the
  index build phase 1 was first blamed for.
- **Phase 2.** Charge the derived structures those buffers feed — the join hash
  index, the group-by hash table, the top-k heap. Proportional to what phase 1
  already charges, so phase 1 bounds them within a constant; phase 2 makes the
  number honest.
- **Phase 3.** A server-wide pool with per-query fair-share, replacing
  "per-query ceiling × concurrency" as the worst case.
- **Phase 4.** Spill-to-disk for sort and hash build, turning a refusal into a
  slower answer where the operator can be made restartable.

## Acceptance criteria

1. A query whose blocking operator would exceed `max_query_memory` fails with
   the typed error and HTTP 507; the response is never a truncated result.
2. A query with no blocking operator is unaffected by the ceiling, at any
   result size. This is a claim about the *charge*, not about memory held:
   such a query still materializes its whole BGP scan (`ScanOp`,
   `scan_bgp_ids`) uncharged, so "unaffected by the ceiling" must not be read
   as "bounded footprint" — see "What the charge does and does not cover".
3. A rejected charge leaves the budget unchanged, and a completed query leaves
   the thread's budget at zero — a second query on a reused blocking-pool
   thread starts from a clean charge.
4. The built-in default is 8 GiB and is reported by the config layer as such;
   `None` remains expressible and means unbounded.
5. `horndb_sparql_query_memory_peak_bytes` is observed exactly once per query,
   on every exit path.
6. The charge tracks what a blocking operator holds, not a fixed trip-wire.
   For a `GROUP BY` whose aggregate the count pushdown cannot serve — so
   `GroupOp` drains its whole input — the query's peak charge grows in
   proportion to the input: ten times the rows charges about ten times the
   bytes. A ceiling set between the charge for N rows and the charge for 10N
   rows admits the first query and refuses the second. **This is the
   criterion that proves the charge points are the right ones:** a budget
   that only trips at one byte, or that reads the same for 1 000 rows as for
   10 000, is measuring something other than the rows the operator keeps.
7. The boundary with the store-side bound is pinned. The single-predicate
   `COUNT` that motivated this spec charges **0** bytes to its query, and any
   footprint it adds shows up in `HornBackend::memory_split().snapshots`, not
   in `horndb_sparql_query_memory_peak_bytes`. A change that starts charging
   store-side memory to a query fails this criterion. This holds on the
   count-pushdown fast path (`count_bgp`/`count_bgp_grouped`); when a backend
   has no `count_bgp_grouped`, `fallback_group_counts` materializes an
   uncharged scan batch and hash map that show up in neither series (see
   "What the charge does and does not cover") — the criterion says nothing
   about that path.
