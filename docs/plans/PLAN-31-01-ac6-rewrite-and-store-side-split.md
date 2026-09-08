---
status: executed
date: 2026-09-08
scope: "SPEC-31 P1 follow-up — rewrite acceptance criterion 6 so it can hold after HDB-229, prove every acceptance criterion with laptop-runnable tests, correct the docs that called the 16.6 GiB query-side memory, and split store-side footprint bounding into HDB-231"
---

# SPEC-31 P1 — Rewrite AC6, prove the criteria, split the store-side bound

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** SPEC-31's enforcement is on this branch. Its acceptance criterion 6
named a demonstration — the LDBC SPB `COUNT` that grew the server by 16.6 GiB —
that HDB-229 has since shown was never operator memory. It was a whole-graph
secondary index, and after HDB-229 that query allocates almost nothing. So AC6
as written can never be met by any budget. This plan replaces it with a
demonstration that holds, proves every criterion with tests that run on a
laptop, fixes the prose that carried the wrong framing, and hands store-side
footprint growth to HDB-231.

**Decisions this plan rests on (made, not open):**

- D1. Work continues on branch `HDB-167-spb-sf-0-256-smoke-killed-hornbench-conf`.
- D2. "Split it": SPEC-31 stays scoped to per-query operator memory. Store-side
  footprint growth gets a separate bound, filed as **HDB-231**.

**Branch state, verified 2026-09-08 (post-rebase):**

- `3b51af41` — enforcement: `crates/sparql/src/exec/budget.rs` (new),
  `exec/op/blocking.rs`, `exec/batch.rs`, `error.rs`, `server/query.rs`,
  `crates/config/src/model.rs`, `crates/metrics/src/sparql.rs`, tests, docs,
  `docs/specs/SPEC-31-query-memory-accounting.md` (new).
- `a373816c` — records that the bound does not bind the SPB `COUNT`, marks AC6
  unmet, and writes "~16.6 GiB is the query's own execution" into
  `docs/benchmarks.md`, `docs/architecture.md` and SPEC-31.
- HDB-229 is on `main` (`27fac41c`) and this branch has been rebased onto
  `7d75804b`, so Task 0's rebase is **already done**.

## What HDB-229 established (the facts the spec must now carry)

- `?cwUri a cwork:CreativeWork` binds predicate and object, so the trie serves
  it from an object-major ordering.
- Producing one materialized the object-major `(o, s)` layout of **every**
  predicate partition in the graph (direct source), or derived a second
  whole-scope ordering from the `Pso` anchor (default memoised
  `VecTripleSource`). 32 B/row over the whole graph, retained for the life of
  the process. That is the 16.6 GiB.
- It is store-side: it lives in `HornBackend`'s snapshot memo / direct-source
  cache, is reused by later queries, and survives the query that built it.
  `HornBackend::memory_split().snapshots` is where it shows up.
- HDB-229 fixed both triggers: `count_bgp` now answers a single-predicate BGP
  off its partition, and the direct source builds leaves only for the
  predicates the BGP names (`bgp_predicates` → `StoreTripleSource::for_predicates`).
- HDB-230 (open, `design`) covers what is left: the default read path still
  derives a second whole-scope ordering for a non-count `?s <p> <o>` pattern.

## Which operators the budget charges (verified in `exec/op/blocking.rs`)

`drain(op, res)` charges every chunk via `Reservation::grow(chunk_bytes(..))`
before appending it. Callers: `JoinOp` (build side = right), `MinusOp` (right),
`LeftJoinOp` (right), `GroupOp` (child), `OrderByOp` (child, both full sort and
`top_k`), `PathClosureOp` (edge child). `UnionOp` charges inline, per mapped
chunk, for both children. Nothing else charges. The `CountScan` /
`GroupCountScan` pushdowns (`plan/pushdown.rs::is_plain_count`: a non-DISTINCT
`COUNT(*)` or `COUNT(?v)` with `?v` bound by the BGP) bypass `GroupOp` and so
charge nothing. A `MAX`, `COUNT(DISTINCT …)`, or a `FILTER` between the BGP and
the `Group` keeps the plan on `GroupOp`.

Over HTTP, every blocking operator drains on its first `next()`, and
`stream_select` pulls chunk 1 before committing headers, so a budget refusal on
the streaming path surfaces as a clean 507 before any body. (Reasoned from
`server/query.rs::stream_select`, not exhaustively tested — flagged in Task 3.)

## Tasks

### Task 0 — Rebase onto `main` — DONE

- [x] `git fetch origin`; `origin/main` is `7d75804b` (HDB-227), which already
      contains HDB-229 at `27fac41c`.
- [x] `git rebase origin/main` — clean, 23 commits replayed. The only shared
      file with HDB-229 was `docs/architecture.md`, and the edits are on
      different rows.
- [x] Verified: `cargo nextest run -p horndb-sparql --features server` — 910
      passed, 3 skipped, including HDB-229's `count_one_partition` and
      `direct_source_leaf_restriction`.

### Task 1 — Amend SPEC-31

File: `docs/specs/SPEC-31-query-memory-accounting.md`. Frontmatter: keep
`date: 2026-09-08`, keep `status: draft`, and update `scope:` to end with "…and
why store-side index growth is bounded elsewhere". Follow the plain-language
rules in the root `CLAUDE.md`.

- [ ] **Problem section, the measurement table.** Keep the table. Replace the
      paragraph that starts "**The growth has two distinct sources, and only
      one is this spec's.**" with:

      > **Neither number in the A/B was query memory.** HDB-229 measured where
      > the growth went. The ~24 GiB is the memoised whole-scope
      > `VecTripleSource` that the first query on a commit version builds and
      > every later query reuses. The ~16.6 GiB was a second index: the query
      > binds predicate and object, so the trie reads an object-major
      > ordering, and building one laid out the `(o, s)` columns of every
      > predicate partition in the graph. Both are store-side — built once,
      > kept for the life of the process, shared by later queries. The
      > executor holds none of it. HDB-229 removed the second index for this
      > shape; HDB-230 tracks what remains of the first. See "What this spec
      > does not bound, and why" below.

      Delete the sentence "That second number is what a per-query budget must
      bound, and ~3.6 kB per counted row is its own defect (HDB-229)".

- [ ] **Non-goals.** Extend the "Bounding the store, dictionary or index
      memory" bullet:

      > This includes index-like structures a query *triggers* but does not
      > own: the snapshot memo, derived orderings, and direct-source leaves.
      > They are amortised across queries, so a per-query budget is the wrong
      > instrument for them (see below). They get a separate bound, HDB-231.

- [ ] **New section after "What the charge does and does not cover", titled
      "What this spec does not bound, and why".** Content, in this order:
      1. What the 16.6 GiB was (three sentences, as in the Problem paragraph
         above, plus the path split: direct source = leaves for every
         predicate; memoised source = a second whole-scope ordering derived
         from the `Pso` anchor).
      2. Why a query budget must not charge it: *the bound would depend on
         arrival order.* The first query to need the ordering would be refused
         at 8 GiB; an identical second query would be served from the index
         the first was refused for building. A per-query limit has to be a
         property of the query alone. Store-side growth needs a bound whose
         unit is the store — a ceiling on the memo, or a decision not to build
         — not a charge to whichever query arrives first.
      3. Where it is tracked: HDB-230 (do not derive the second ordering on
         the default path) and HDB-231 (the store-side ceiling).
      4. One sentence that after HDB-229 the demonstration query allocates
         almost nothing on either path, which is why the original AC6 was
         replaced rather than kept open.

- [ ] **Also add to the "Not counted" list** in "What the charge does and does
      not cover": *the materialized result of the non-streaming path* —
      `execute_query`'s `QueryAnswer::Solutions { rows }` and the server's
      `run_materialized` (ASK/CONSTRUCT/DESCRIBE/EXPLAIN) collect every row
      before serializing, and that collection is not charged. Streaming SELECT
      is bounded by `max_result_rows`; the materialized path is bounded by
      neither. Record it as a known gap (phase 2 candidate). This was found by
      reading, not measured.

- [ ] **Replace the whole section "Status — phase 1 does not yet bound the
      query that motivated the spec"** with "Status — phase 1 bounds what it
      says it bounds". Keep the measurement table (it is a true record: peak
      charge 0, RSS +40.7 GiB) and re-read it: the zero was correct, not a
      gap. The memory was in the store's memo, and the instrument said so.
      Point to the tests in Tasks 2–4 as the proof of each criterion. Drop the
      paragraph "Finding and charging the 16.6 GiB is HDB-229, and it is the
      gate on calling `max_query_memory` a real bound"; replace with: the
      cgroup ceiling (`MEMORY_MAX`) remains the host guard against
      *store-side* growth until HDB-231 lands.

- [ ] **Phases.** Delete "Phase 1.5 (HDB-229, open)". Add one line under Phase
      1: "Phase 1 landed; HDB-229 (merged) removed the index build it was first
      blamed for." Leave phases 2–4 as they are.

- [ ] **Acceptance criteria.** Replace criterion 6 and add 7:

      > 6. The charge tracks what a blocking operator holds, not a fixed
      >    trip-wire. For a `GROUP BY` whose aggregate the count pushdown
      >    cannot serve — so `GroupOp` drains its whole input — the query's
      >    peak charge grows in proportion to the input: ten times the rows
      >    charges about ten times the bytes. A ceiling set between the charge
      >    for N rows and the charge for 10N rows admits the first query and
      >    refuses the second. **This is the criterion that proves the charge
      >    points are the right ones:** a budget that only trips at one byte,
      >    or that reads the same for 1 000 rows as for 10 000, is measuring
      >    something other than the rows the operator keeps.
      > 7. The boundary with the store-side bound is pinned. The
      >    single-predicate `COUNT` that motivated this spec charges **0**
      >    bytes to its query, and any footprint it adds shows up in
      >    `HornBackend::memory_split().snapshots`, not in
      >    `horndb_sparql_query_memory_peak_bytes`. A change that starts
      >    charging store-side memory to a query fails this criterion.

- [ ] Verify: read the file top to bottom once; every sentence about the
      16.6 GiB now says "index" or "store-side", none says "the query's own
      execution".
      `grep -n "query's own execution\|query-side" docs/specs/SPEC-31-query-memory-accounting.md`
      returns nothing.

### Task 2 — API-level tests for AC6 and AC7 (new file)

File: `crates/sparql/tests/query_memory_budget.rs` (new). Uses the real
`HornBackend` so rows are `Slot::Id` from the real scan path, and
`horndb_sparql::exec::budget`. `execute_query` runs on the calling thread, so a
`budget::scope` installed in the test is the query's scope and `budget::peak()`
is readable afterwards.

Fixture: `fn store(n: u64) -> HornBackend` — `n` triples
`<http://ex/s{i}> <http://ex/p> <http://ex/o{i}>` via `insert_oxrdf_batch`
(pattern: `count_one_partition.rs::fixture`).

Query under test (must stay on `GroupOp`; `MAX` is not a plain count, so
`is_plain_count` is false and no pushdown fires):

```sparql
SELECT ?p (MAX(?o) AS ?m) WHERE { ?s ?p ?o } GROUP BY ?p
```

- [ ] `group_by_charge_grows_with_the_input` (AC6, first half).
      For `n` in `[2_000, 20_000]`: `let _g = budget::scope(None);` run the
      query, record `budget::peak()`, drop the guard. Assert `peak(2_000) > 0`
      and `8 * peak(2_000) <= peak(20_000) <= 12 * peak(2_000)`. Why the
      window: `drain` charges each chunk as
      `len × size_of::<Row>() + Σ Row::heap_bytes`, which is linear in rows;
      the window absorbs the unlikely case that the planner projects a
      different width for the two sizes. **Fails without enforcement:** with no
      `grow` calls `peak()` is 0 and the first assert fails.
- [ ] `a_ceiling_between_two_sizes_admits_the_small_and_refuses_the_large`
      (AC6, second half). Measure `small = peak(2_000)` unbounded. Then with
      `budget::scope(Some(small * 2))`: the 2 000-row query returns `Ok`, the
      20 000-row query returns
      `Err(SparqlError::QueryMemoryLimit { limit, requested })` with
      `limit == small * 2` and `requested > limit`. Also assert
      `budget::used() == 0` after the error (AC3: a rejected charge and a
      dropped operator tree leave nothing behind). **Fails without
      enforcement:** the 20 000-row query returns `Ok`.
- [ ] `the_pushdown_count_charges_nothing_to_the_query` (AC7). Fixture as in
      `count_one_partition.rs` (one small predicate `p0` with a shared object,
      two large predicates). Under `budget::scope(Some(1))` (one byte), run
      `SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/p0> <http://ex/o> }` on
      the default (memoised) backend. Assert `Ok`, count is correct,
      `budget::peak() == 0`, and `memory_split().snapshots` grew (assert only
      `after > before`). This pins the split: the memory the query triggers is
      in the store, the query's charge is zero, and a one-byte budget does not
      refuse it. **Fails if** anyone charges the memo to the query (peak > 0 or
      `Err`), or if the memo stops being the place the growth lands.
- [ ] Verify:
      `cargo nextest run -p horndb-sparql --features server --test query_memory_budget`.
      Sanity-check the RED: temporarily comment out the `res.grow(...)` line in
      `blocking.rs::drain`, run again, confirm tests 1 and 2 fail, restore.

### Task 3 — HTTP tests: one per blocking operator, plus a sized ceiling

File: `crates/sparql/tests/server_http.rs`, module `spec26_query_settings`
(helpers `router_with_rows`, `get`, `SELECT_ALL` already there). `ByteSize`
parses `1MiB` (`crates/config/src/units.rs`), so URL values can be readable.

Existing: `max_query_memory_refuses_a_blocking_operator_over_budget` (ORDER BY
at 1 byte), `max_query_memory_does_not_refuse_a_streaming_query` (AC2),
`the_default_budget_admits_an_ordinary_sort` (AC4 via HTTP). Add:

- [ ] `max_query_memory_sized_ceiling_group_by` (AC6 over HTTP). Query
      `SELECT ?p (MAX(?o) AS ?m) WHERE { ?s ?p ?o } GROUP BY ?p` with
      `&max_query_memory=1MiB`: `router_with_rows(500, …)` → 200;
      `router_with_rows(50_000, …)` → 507 with body containing
      `query memory limit exceeded`. Margins: `MemStore` rows come through
      `Batch::from_bindings` as `Slot::Term` strings; 500 rows are well under
      1 MiB even at 1 kB/row, and 50 000 rows exceed it even at 24 B/row.
      **Fails without enforcement:** 50 000-row case returns 200.
- [ ] One 1-byte refusal per remaining funnel entry, each on
      `router_with_rows(5_000, Limits::default())`, each asserting 507 and the
      error text. They exist so a future change that drops one operator's
      `Reservation` is caught:
      - `UnionOp`: `SELECT ?s WHERE { { ?s ?p ?o } UNION { ?s ?p ?o } }`
      - `LeftJoinOp` build side:
        `SELECT ?s ?o2 WHERE { ?s <http://ex/p> ?o OPTIONAL { ?s <http://ex/p> ?o2 } }`
      - `MinusOp` build side: `SELECT ?s WHERE { ?s ?p ?o MINUS { ?s ?p ?o } }`
      - `PathClosureOp`: `SELECT ?s ?o WHERE { ?s <http://ex/p>+ ?o }`
      - `JoinOp` build side: a subselect join,
        `SELECT ?s WHERE { ?s ?p ?o . { SELECT ?o WHERE { ?x <http://ex/p> ?o } } }`.
        If the planner folds this into one BGP scan (no `JoinOp`), drop this
        case and say so in the test file header; the `JoinOp` `Reservation` is
        then covered only by Task 2's proportionality (it shares `drain`).
      **Fails without enforcement:** each returns 200.
- [ ] Flag, not a task: a 507 *after* headers commit was not constructed. Every
      blocking operator drains on its first `next()` and `stream_select`
      pre-buffers chunk 1 before committing, so no shape was found that trips
      mid-stream. If one is found, it must abort the body like
      `max_result_rows_over_cap_mid_stream_aborts_the_body`.
- [ ] Verify:
      `cargo nextest run -p horndb-sparql --features server --test server_http spec26_query_settings`.

### Task 4 — AC5: the peak metric is observed exactly once per query

File: `crates/sparql/tests/query_memory_metrics.rs` (new, **one test in its own
binary**). Reason: `horndb_metrics::metrics()` is process-global; under
`cargo nextest` every test is its own process, but `cargo test` runs a binary's
tests in parallel threads, and a count-delta assertion shared with other tests
would flake. The repo already isolates process-global state this way
(`tests/graph_store_materialized.rs`).

- [ ] Build a router the way `spec26_query_settings::router_with_rows` does
      (copy the ~15 lines: `MemStore`, `build_router`, `AppState { store,
      config: ConfigHandle::from_limits(Limits::default()), ready, admission }`).
- [ ] Read `horndb_sparql_query_memory_peak_bytes_count` and
      `horndb_sparql_queries_over_budget_total` from
      `horndb_metrics::encode_metrics()` (a `parse_counter`-style line parser;
      the one in `server_http.rs` is private, copy it). Then issue, in order:
      (a) the streaming `SELECT_ALL` (200, peak 0), (b) the ORDER BY at
      `max_query_memory=1` (507), (c) the ORDER BY at the default (200). Assert
      `_count` grew by exactly 3 and `over_budget_total` by exactly 1.
- [ ] The observation happens in `budget::Scope::drop` on the blocking-pool
      thread, which can run *after* the response is delivered. So poll
      `encode_metrics()` for up to 2 s until the expected count appears, then
      assert. Without the poll this test is racy; say so in a comment.
- [ ] **Fails without** the `observe` in `Scope::drop`: the count does not
      move. Fails if a refusal path stops observing: delta is 2, not 3.
- [ ] Verify:
      `cargo nextest run -p horndb-sparql --features server --test query_memory_metrics`
      and once with
      `cargo test -p horndb-sparql --features server --test query_memory_metrics`.

### Task 5 — Correct the prose that called the 16.6 GiB query memory

Every passage below was read; edit these and nothing more. Root `CLAUDE.md`
doc-sync rule: `docs/architecture.md` changes ride this PR; `TASKS.md` is
main-only and is not touched on a feature branch.

- [ ] `docs/architecture.md`, row "`max_query_memory` enforcement". Delete the
      sentence starting "**Measured gap:** the query that motivated it
      (HDB-167's COUNT) charges 0 bytes while growing the server 40 GiB…".
      Replace with: "The query that motivated it charges 0 bytes, correctly:
      HDB-229 showed its 40 GiB was a whole-graph index build (store-side,
      amortised), which a per-query budget must not charge (SPEC-31, 'What this
      spec does not bound'). Store-side growth gets its own bound: HDB-231."
      Keep "implemented (phase 1)".
- [ ] `docs/architecture.md`, row "LDBC SPB nightly throughput A/B". Replace
      "HornDB enforces no per-query memory bound (`max_query_memory` is parsed
      but not applied), so `start-engine.sh` now also takes `MEMORY_MAX`…" with
      "`max_query_memory` bounds the executor's row buffers (SPEC-31), not the
      store's index growth, so `start-engine.sh` also takes `MEMORY_MAX`…".
      Append to "One query in the driver's parameter-sampling phase grew the
      server from 24 GiB to the 90 GiB ceiling and the cgroup killed it (host
      unaffected)": "— a whole-graph secondary-index build, fixed in HDB-229,
      not query memory".
- [ ] `docs/benchmarks.md`, section "First run at SF=0.128". Replace "That is
      ~66 GiB of query-side memory on a 234 M-triple store, and it settles
      HDB-167's open question in a controlled way: the failure is real, it is
      query-side, and halving the corpus does not avoid it." with "That is
      ~66 GiB of growth on a 234 M-triple store. It settles HDB-167's open
      question: the failure is real and halving the corpus does not avoid it.
      (It was later attributed — see the next section — to a whole-graph index
      build, not to the query's own buffers.)" And to "A benchmark reading
      still needs a real bound on query memory (HDB-167 deliverable 2);
      `max_query_memory` remains unenforced." add "(landed in SPEC-31 on this
      branch)". Keep the rest: this section is a dated log.
- [ ] `docs/benchmarks.md`, section "Which query, and where the memory goes".
      Replace the bullet "**~16.6 GiB is the query's own execution** — ~3.6 kB
      per counted row… it is what SPEC-31's per-query budget has to charge."
      with "**~16.6 GiB was a second index.** The pattern binds predicate and
      object, so the trie reads an object-major ordering; building one laid out
      the `(o, s)` columns of every predicate partition in the graph (HDB-229).
      Store-side, retained, not the query's own buffers. HDB-229 removed the
      build for this shape." Replace the paragraph "**SPEC-31's budget does not
      currently charge it.**…does not yet cover this shape." with: keep the
      measurement (peak 0, RSS +40.7 GiB, 200 at 8 GiB), then "That zero was
      the instrument reading correctly: none of the growth was in an operator
      buffer. Bounding store-side growth is HDB-231; the cgroup ceiling stays
      the host guard until it lands." Leave the "correction to how serving
      footprint is recorded" paragraph untouched — it is still true.
- [ ] `crates/harness/scripts/start-engine.sh` (~line 109): "HornDB does not
      bound the memory a query may use: `[server.limits].max_query_memory` is
      parsed and carried but never enforced (SPEC-26 S5 non-goal)." → "SPEC-31
      bounds the executor's row buffers per query; it does not bound the
      store-side index and snapshot memory a query can trigger (HDB-229,
      HDB-230, HDB-231)." Keep the rest of the comment.
- [ ] `.github/workflows/nightly.yml` (~line 60): "HornDB does not enforce a
      per-query memory bound (`[server.limits].max_query_memory` is parsed but
      not applied)" → "`max_query_memory` (SPEC-31) bounds only the executor's
      row buffers, not store-side index growth". Pinned-SHA rules in
      `.github/CLAUDE.md` are unaffected (comment-only change).
- [ ] `scripts/bench/spb-scale-smoke.sh` (~line 9): same substitution.
- [ ] `docs/index.md`: change "Status: **draft, phase 1 landed** (HDB-167)" to
      "Status: **draft, phase 1 landed and all seven criteria tested**
      (HDB-167); store-side growth is bounded separately (HDB-231)".
- [ ] `docs/specs/SPEC-26-config-system.md` (Non-goals): append one sentence
      "Enforcement landed as SPEC-31." — a pointer only, no re-scoping.
      Optional; skip if the coordinator prefers SPEC-26 untouched.
- [ ] Unchanged on purpose (verified to carry no wrong claim):
      `crates/sparql/INTEGRATION-NOTES.md`, `docs/metrics.md`,
      `docs/specs/README.md`, `crates/sparql/src/exec/budget.rs` header,
      `crates/config/src/model.rs` doc comments.
- [ ] Verify:
      `grep -rn "query's own execution\|is parsed but not applied\|never enforced\|no per-query memory bound\|enforces no per-query" docs crates/harness/scripts .github/workflows scripts/bench`
      returns nothing except the dated log lines in `docs/benchmarks.md` that
      were annotated above.

### Task 6 — Commit shape and checks

- [ ] Three commits: (1) spec amendment + tests (Tasks 1–4), (2) doc and script
      corrections (Task 5), (3) plan file `status: executed` flip. Each with
      `Worklode-Task: HDB-167` in the trailer.
- [ ] `cargo fmt --all -- --check`;
      `cargo clippy -p horndb-sparql --all-targets --features server -- -D warnings`;
      `cargo nextest run -p horndb-sparql --features server`;
      `cargo nextest run -p horndb-config`.
- [ ] `graphify update .` if `graphify-out/graph.json` exists (root `CLAUDE.md`).

### Task 7 — Store-side bound task — DONE

- [x] Filed as **HDB-231** ("Bound store-side memory a query can trigger:
      snapshot memo, derived orderings, direct-source leaves"), kind `design`,
      priority `high`, concern `performance`, `follow_up_to HDB-167`. Its body
      carries the five-item deliverable and explains how it differs from
      HDB-230: HDB-230 is a *reduction* (build less), HDB-231 is a *ceiling*
      (bound whatever is built). Neither subsumes the other.

### Task 8 — Merge order — resolved

- [x] Nothing to sequence: HDB-229 merged to `main` at `27fac41c` before this
      plan was written, and this branch is rebased onto `7d75804b`.

## Things not verified by reading (flagged, not guessed)

- Whether repo specs/plans are expected to be registered with `lode doc add`.
  `lode doc list` numbers do not follow the repo's `SPEC-NN` numbering, and
  `docs/specs/AGENTS.md` does not mention it. Ask before registering.
- `size_of::<Slot>()` and the exact per-row charge. Every test above uses
  ratios or wide margins so this does not matter; do not tighten them to exact
  byte counts.
- A mid-stream 507 (Task 3 flag).
- Whether the subselect-join shape in Task 3 lowers to a `JoinOp` or is
  flattened; the task says what to do either way.
- The materialized-path collection gap (Task 1, "Not counted") is from reading
  `server/query.rs::run_materialized` and `api.rs`, not measured.
