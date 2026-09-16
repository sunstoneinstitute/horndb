---
status: draft
date: 2026-09-08
scope: "SPEC-31 — what `max_query_memory` bounds: per-query charging of the executor's row buffers (blocking operators, the BGP scan, DISTINCT's seen-set, path-closure output, hash-join fan-out), the refusal contract when a query crosses its ceiling, the default, what each later phase adds to the accounting; and (S6) the separate store-side ceiling `max_snapshot_memory` puts on the snapshot memo"
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
  below). They get a separate bound: **S6**, added by HDB-231.
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
normalize across both children) and charges the same way, then tops its charge
up after `normalize_columns` — that step rewrites `Slot::Id` cells into decoded
`Slot::Term` strings, so the rows it retains are larger than the rows it first
charged.

A **streaming** operator that keeps no state across chunks holds one chunk at
a time whatever the result size, and charges nothing. **Not every operator
outside the table above is such an operator.** An earlier version of this
section said the opposite — that anything not blocking is "the bound working
as specified, not a gap" — and that was wrong. Four more accumulations are
per-query memory that grows with the data, and HDB-232 charges them too:

| accumulation | what it holds |
|---|---|
| the BGP scan (`ScanOp`, `HornBackend::scan_bgp_ids`) | the whole materialized scan, for the query's life |
| `DistinctOp`'s `seen` set | one key per distinct row, for the query's life |
| `PathClosureOp`'s output | the computed closure, which can be quadratic in the edge set it drained |
| `pending` on `JoinOp` / `LeftJoinOp` / `MinusOp` | one probe chunk's matched-row fan-out |

The scan is charged in two places, on purpose. `scan_bgp_ids` charges each
WCOJ output batch as it appends it, so a scan that would cross the ceiling is
refused while it is being built rather than once the whole thing exists; that
charge is released when the scan returns. `ScanOp` then charges the finished
batch and holds it for the operator's life, because the buffer stays allocated
however many chunks have been handed out.

One consequence: a scan feeding a blocking operator is charged about twice —
once by `ScanOp`, which is still holding, and again by the parent as it drains
the rows. That reads high, never low, and the alternative (releasing the scan's
charge chunk by chunk) costs a byte-counting pass per chunk on the executor's
hottest path.

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

### S6. A separate ceiling on the snapshot memo (HDB-231)

The sections above bound what a query *owns*. This one bounds what a query
*triggers* and the store then *keeps*: the memoised whole-scope
`VecTripleSource` that the first query on a commit version builds, that every
later query reuses, and that no query can be charged for. See "What this spec
does not bound, and why" for why a per-query budget is the wrong instrument
for it.

**The knob.** `[server.limits].max_snapshot_memory`, server scope, **not**
per-query overridable — it is not in `QuerySettings`, because no client can
be billed for the memo or fix it by sending a different request. `None` is
unbounded and stays expressible. It sits in `[server.limits]` and not a new
`[storage]` section for two reasons: SPEC-26 already puts the server-scope,
non-overridable limits there (`max_concurrent_queries`, `queue_timeout`,
`max_request_body`), and the memo is built by the SPARQL execution layer, not
by `horndb-storage`. It is restart-only — `serve` installs it on the backend
at startup — and `horndb_config::restart_only_changes` says so on reload.

**The default is 64 GiB.** Unlike `max_query_memory`, this ceiling is sized
by the corpus, so a default has to admit the corpora HornDB is built to serve
and refuse only runaway growth. On the measured LDBC SPB series, SF=0.128
(234 M triples) estimates at ~33 GiB and is admitted; SF=0.256 estimates at
~67 GiB and is refused — and SF=0.256 is the run that exhausted a 124 GiB
host on 2026-09-05. Like `max_query_memory` it is not derived from total RAM;
an operator on a smaller host should lower it.

**Behaviour at the ceiling: refuse the build, before it happens.** A scope
whose snapshot would push the memo past the ceiling fails the query with
`SparqlError::SnapshotMemoryLimit` and HTTP 507. A memo *hit* is never
refused — the check guards the build only, so a ceiling cannot start failing
queries the store already has the index for, and lowering the ceiling never
changes an answer already being served.

Refusal is chosen over the two alternatives because it is the only one of the
three that bounds anything. Evicting the least-recently-used scope first does
not: the build *is* the allocation, so the peak is paid whether or not
something is dropped afterwards, and with at most two whole-store scopes in
the memo the entry evicted is the same size as the one replacing it —
thrashing, not a ceiling. Falling back to the anchor ordering does not bound
it either: the anchor is the ~24 GiB, and the second ordering it would avoid
deriving is the 16.6 GiB HDB-229 already removed on this shape. Refusal also
gives the operator the one thing the other two hide — the error names
`max_snapshot_memory`, what the memo holds, and what the refused entry would
have cost, which is exactly what they need to size it. Silently declining to
memoise would instead make every query pay a full rebuild, a large latency
cliff with no signal at all.

**507, like S2, and for the same reason** — the server declined to spend the
memory, and the request is legal. The two are told apart by the message,
which names the knob that applies: only `max_snapshot_memory` can be raised
to make this query succeed, and a client cannot raise it.

**What is charged.** The estimate is the worst case for the scope: rows x 144
bytes (six orderings x three `TermId` columns x 8 B). The worst case rather
than the current one, because the five non-anchor orderings are derived
lazily, long after the snapshot was admitted, from inside `horndb-wcoj` where
there is no ceiling to consult — so charging all six up front is the only
estimate that still holds for the entry's whole life. It reads high: the SPB
SF=0.128 memo measured ~102 B/triple against this 144. That is the right
direction for a ceiling.

**What it does not bound.** Non-memoisable scopes (`GRAPH <g>`, `FROM` unions
— see `SnapshotScope::memoisable`) are rebuilt per query and freed with it,
so they are not store-side and are not checked here. They are per-query
memory, but not the *row-buffer* kind S1 charges — a rebuilt triple source is
not an operator's buffer — so HDB-232 does not charge them either, and they
remain an open gap. The `direct_cache`
(`StoreTripleSource`) is also outside it: its leaves may be `Arc`-clones of
the partitions' own columns, so it has no byte figure of its own that would
not double-count `partitions` — which is why `MemorySplit` leaves it out too.
The planner's `SnapshotStats` cache is store-side but small, and is not
checked.

**Observability.** `horndb_sparql_snapshot_memo_bytes` — gauge, the bytes the
memo holds, read at scrape time; `docs/metrics.md` carries the row. It is the
same number the ceiling is compared against, so an operator watching the
gauge sees the refusal coming.

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

The remaining **per-query** gaps — rows a query holds for its own duration and
frees when it ends, the same class S1 charges — are small and bounded by
something that *is* charged:

- the grouping hash map in `fallback_group_counts`, the path `GroupCountScan`
  takes when the backend has no `count_bgp_grouped` fast path. Its scan batch
  is charged; the map beside it is not. The map holds one entry per distinct
  key, so the charged batch bounds it;
- `GroupOp`'s and `OrderByOp`'s *output* buffers. Neither can exceed the input
  the operator already charged, so the charge bounds them within a constant.
  (`PathClosureOp`'s output can exceed its input, which is why that one is
  charged in its own right.);
- the per-query triple source a non-memoisable scope (`GRAPH <g>`, a `FROM`
  union) rebuilds — see S6's "What it does not bound". It is sized by the
  scope, not by any operator's row buffer, and nothing charges it.

Writing that down is the point: an operator setting 8 GiB should read it as "no
query's own row buffers may accumulate more than 8 GiB", not as "no query may
add more than 8 GiB of RSS".

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

That bound is **S6** above, added by HDB-231: a configured ceiling on the
snapshot memo, with a refusal before the build and a gauge. It is a ceiling,
not a reduction — HDB-230 (do not derive the second whole-scope ordering on
the default path) is the reduction, and neither subsumes the other. After
HDB-229 the demonstration query allocates almost nothing on either path,
which is why the original acceptance criterion 6 was replaced rather than
kept open.

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
rewritten criterion 6.

HDB-232 then closed the gaps that left: the BGP scan, `DistinctOp`'s seen-set,
the path closure's output, a hash join's probe fan-out, and `UnionOp`'s
under-charge after normalization are all charged now, and
`fallback_group_counts` charges the scan batch it materializes. What is left
uncharged is listed under "What the charge does and does not cover" — each
remaining item is bounded by something that is charged, except the two named
there as open gaps (the materialized result path, and a non-memoisable scope's
rebuilt triple source).

The cgroup ceiling (`MEMORY_MAX` in `crates/harness/scripts/start-engine.sh`)
stays as a belt-and-braces host guard. S6 is what the server itself now
enforces against store-side growth; the at-scale re-measurement that would
show the served footprint no longer climbing past load + ceiling still has to
run on hornbench.

## Phases

- **Phase 1 (this spec, landed).** Charge blocking-operator row buffers; refuse
  over budget; 8 GiB default; the two metrics. HDB-229 (merged) removed the
  index build phase 1 was first blamed for. S6 (HDB-231) landed alongside it:
  the store-side ceiling, which is a different bound on a different unit, not
  a later phase of this one. HDB-232 extended phase 1 to the four per-query
  accumulations S1's second table lists; it is the same bound on the same
  unit, not a later phase either.
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
2. A query with no blocking operator is charged for its BGP scan and for
   nothing per result row on top of that. The scan is the whole materialized
   `Batch` the query holds for its life, so a ceiling below it refuses the
   query and the default ceiling serves it. (This criterion replaced an
   earlier one claiming such a query was unaffected by the ceiling at any
   result size — true of the charge as it then stood, and misleading about
   the memory held. HDB-232 charges the scan instead.)
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
   has no `count_bgp_grouped`, `fallback_group_counts` scans, and that scan
   batch is charged to the query — so the criterion applies to the fast path
   only, and a charge on the fallback path is correct, not a violation.
8. The store-side ceiling (S6) refuses a build, not a hit. With
   `max_snapshot_memory` set below one whole-scope snapshot's worst case, a
   query that would build the memo fails with `SnapshotMemoryLimit` and
   HTTP 507 and leaves `horndb_sparql_snapshot_memo_bytes` unchanged; with
   the memo already warm, the same query is served however low the ceiling
   is set afterwards. `None` remains expressible and means unbounded, and
   the built-in default is 64 GiB.
9. Every per-query accumulation S1 names is charged, and each charge is
   *held* while the rows are held, not merely seen in passing. Because the
   peak is a high-water mark, a charge released early can still show up in
   the metric, so the test for this compares two data sizes: the amount an
   operator adds over the same scan must scale with the data, not sit at one
   chunk's worth. Two stacked blocking operators must charge the sum of what
   they hold, not the larger of the two.
