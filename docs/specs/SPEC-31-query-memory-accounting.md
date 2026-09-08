---
status: draft
date: 2026-09-08
scope: "SPEC-31 — what `max_query_memory` bounds: per-query charging of the executor's blocking-operator row buffers, the refusal contract when a query crosses its ceiling, the default, and what each later phase adds to the accounting"
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

**The growth has two distinct sources, and only one is this spec's.** The A/B
above splits them: ~24 GiB is the memoised whole-scope `VecTripleSource` that
the first query on a commit version builds and every later query reuses
(`HornBackend`'s snapshot memo) — store-side, amortised, and *not* attributable
to the query that happened to trigger it. The remaining **~16.6 GiB is the
query's own execution**, for a COUNT whose answer is one row. That second
number is what a per-query budget must bound, and ~3.6 kB per counted row is
its own defect (HDB-229) — the bound stops the bleeding, it does not explain
it.

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
  corpus, not by a query, and belong to SPEC-02/SPEC-25.
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
in one place for five of the six. `UnionOp` accumulates inline (it must
normalize across both children) and charges the same way.

A **streaming** operator holds one chunk at a time whatever the result size and
so charges nothing. That is the bound working as specified, not a gap: a query
with no blocking operator is already bounded by construction, and result size
is `max_result_rows`' job.

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
  neither of which `Row::heap_bytes` walks — so the charge reads slightly low.

Writing that down is the point: an operator setting 8 GiB should read it as "no
query's blocking operators may accumulate more than 8 GiB", not as "no query
may add more than 8 GiB of RSS".

## Phases

- **Phase 1 (this spec, landed).** Charge blocking-operator row buffers; refuse
  over budget; 8 GiB default; the two metrics.
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
   result size.
3. A rejected charge leaves the budget unchanged, and a completed query leaves
   the thread's budget at zero — a second query on a reused blocking-pool
   thread starts from a clean charge.
4. The built-in default is 8 GiB and is reported by the config layer as such;
   `None` remains expressible and means unbounded.
5. `horndb_sparql_query_memory_peak_bytes` is observed exactly once per query,
   on every exit path.
6. The LDBC SPB parameter-sampling query that grew the server by 16.6 GiB of
   query-side memory (HDB-167) charges that memory to its budget, and is
   therefore refused at the 8 GiB default instead of being served. **This is
   the criterion that proves the charge points are the right ones**: if the
   query's peak charge reads near zero while its RSS grows by gigabytes, the
   budget is measuring something other than where the memory goes.
