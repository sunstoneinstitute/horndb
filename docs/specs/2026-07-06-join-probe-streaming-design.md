# Join probe-side streaming + bound-key join-variable selection — design

**Date:** 2026-07-06
**Issues:** deferred items 1 and 4 of [#128](https://github.com/sunstoneinstitute/horndb/issues/128) (see `TASKS.md` "Remaining / deferred work").
**Status:** design — implementation plan: `docs/plans/2026-07-06-join-probe-streaming.md`.

## Problem

Two deferred defects live in the same join code (`crates/sparql/src/exec/op/blocking.rs`
+ `crates/sparql/src/exec/runtime.rs`), so they are designed and landed together:

- **Joins are not streaming.** `JoinOp`/`LeftJoinOp` (blocking.rs:114-172) drain
  **both** children fully before emitting their first output tuple, then hand the
  whole-batch `compute_join`/`compute_left_join` result out via `ChunkedBatch`.
  The #143 design (`docs/specs/2026-06-30-streaming-runtime-pushdown-design.md` §2)
  intended: build a hash table from the build side on first `next()`, then stream
  the probe side chunk-by-chunk. The eager first cut was kept because the SPB query
  mix never exercises a large probe side under a join — but it blocks full
  end-to-end streaming: any plan with a join above a scan re-materializes the
  scan's entire output.
- **`batch_join_vars` keys on schemas, not bound values** (runtime.rs:773-778): the
  join-variable set is the intersection of the two child *schemas*. `row_join_key`
  (runtime.rs:711-731) returns `None` for any row whose key contains an `Unbound`
  slot, sending that row to the conservative `unkeyed` bucket that every probe row
  must scan. A shared variable that is unbound in **every** build-side row (e.g. an
  OPTIONAL-produced column, or `VALUES` with `UNDEF`) therefore unkeys the *entire*
  build side and degrades the hash probe toward O(|l|·|r|). Results stay correct
  (`merge_rows` re-checks compatibility pair-wise); only the complexity collapses.

## Non-goals

- **Streaming or spilling the build side.** The right child is still drained fully
  into memory on the first `next()` — same memory profile as today's right side.
  No grace/hybrid hash join, no spill-to-disk.
- **Cost-based side selection.** Build stays right, probe stays left,
  unconditionally (matching the current planner's 1:1 lowering, which has no
  cardinality model). A future planner rewrite may swap children; the operator
  does not.
- **Streaming `Union`.** `UnionOp` keeps its eager drain — its correctness note
  (blocking.rs module doc) is exactly the chunk-provenance problem this design
  solves for joins, but Union is not on the #128 path and stays as-is.
- **Adaptive re-keying.** If a shared variable is unbound in every **probe**-side
  row (left), those rows still take the probe-all path — the key set is fixed when
  the index is built and the probe side is, by design, never pre-scanned. See
  Risks.
- **Streaming results out to HTTP** (`Runtime::run` still collects `Vec<Bindings>`)
  and **filter-aware/grouped count pushdown** — deferred items 2 and 3 of #128,
  untouched here.

## Current model (baseline)

- **`Op` contract** (op/mod.rs:37-40): `schema()` + `next() -> Result<Option<Batch>>`,
  chunks of ≤ `batch_rows()` rows, never a `Some(empty)` mid-stream.
- **`JoinOp`/`LeftJoinOp`** (blocking.rs:93-172): on first `next()`, `drain` both
  children, call `Runtime::compute_join` / `compute_left_join` (runtime.rs:375-548),
  buffer the whole result in a `ChunkedBatch`.
- **Hash machinery** (runtime.rs): `batch_join_vars` (schema intersection, sorted)
  → `row_join_key` (decoded lexical key per row; `None` if any jvar unbound) →
  index `HashMap<Vec<String>, Vec<&Row>>` + `unkeyed` bucket → per-left-row probe
  via `merge_all`/`probe_into_slots`, with `merge_rows`/`merge_rows_with` as the
  pair-wise compatibility arbiter → `normalize_columns` over the **full** output
  row set.
- **Homogeneity invariant** (batch.rs module doc): within a batch, a column never
  mixes `Slot::Id` and `Slot::Term` (`Unbound` may appear anywhere), so equality
  keys (`Slot::key_part`) can hash raw ids. Crucially, **cross-chunk consumers
  already rely on the stream-wide version of this invariant**: `DistinctOp`'s
  seen-set and `GroupOp`'s key map span chunks, and `KeyPart::Id(x) ≠
  KeyPart::Lex(lex(x))` for the same logical term. Today the stream-wide form
  holds because Join/Union normalize over their *entire* output before chunking —
  which is precisely what a streaming join can no longer do.

## Design

### 1. Build/probe sides

**Right = build, left = probe, for both `Join` and `LeftJoin`.**

- `LeftJoin` has no choice: the probe side must be the required (left) pattern so
  an unmatched left row can be emitted immediately with `Slot::Unbound` right-only
  columns. Building on the left would require tracking per-left-row match flags
  until the right stream ends — blocking again.
- `Join` keeps the same orientation for parity with today's `compute_join` (right
  was already the indexed side) and with `LeftJoin`, so both ops share one build
  state and one probe helper.

### 2. Operator state machine

A shared `JoinState`, built by `Runtime::build_join_state` on the first `next()`:

```text
JoinState {
    build:       Batch                             // drained right child
    index:       HashMap<Vec<String>, Vec<usize>>  // row indices into build.rows
    unkeyed:     Vec<usize>                        // build rows with an unbound jvar
    jvars:       Vec<Var>                          // §4: bound-key selection
    out_schema:  Vec<Var>                          // union_schema(left, right)
    merge_plan:  Vec<(Option<usize>, Option<usize>)>
    forced_term: Vec<bool>                         // §3: per-output-column decode
}
```

The index stores **row indices**, not `&Row`, because the state owns `build`
(a self-referential `Vec<&Row>` cannot live next to the `Batch` it borrows —
the current borrow-local `HashMap<_, Vec<&Row>>` only works inside a single
function call).

`next()` then loops:

1. Serve from `pending` (a `ChunkedBatch` carry) if non-empty. One probe chunk
   can fan out to **more** than `batch_rows()` merged rows; the carry keeps the
   ≤ `batch_rows()` chunk contract.
2. Pull one probe (left) chunk; `None` ⇒ end of stream. Probe every row against
   `index` + `unkeyed` (or against all build rows when the probe row's own key is
   unbound), exactly today's per-row logic. `LeftJoin` additionally applies the
   OPTIONAL `FILTER` per merged row and emits `left ⋈ all-Unbound` for an
   unmatched probe row — the matched flag is per-probe-row, so no state spans
   chunks besides `JoinState` itself.
3. Enforce column provenance (§3) on the chunk's output rows, then stash them in
   `pending` and serve. Empty output for a chunk ⇒ pull the next probe chunk
   (never emit `Some(empty)`).

Fast path: an **inner** `Join` whose build side drained empty ends the stream
without pulling the probe side at all (`LeftJoin` must still stream the left
side through, emitting all-Unbound right columns).

`compute_join`, `compute_left_join`, `batch_join_vars`, `merge_all`, and
`probe_into_slots` are deleted; `merge_rows`/`merge_rows_with`/`row_join_key`/
`build_merge_plan`/`union_schema` are reused unchanged (modulo index-based
candidate lists).

### 3. Chunk-spanning provenance: `Op::may_emit_term` + forced columns

Per-chunk `normalize_columns` is **not** a substitute for today's full-output
normalize. Counter-example (chunk size 1): probe = `VALUES ?v { UNDEF <v1> }`
(Term provenance), build = BGP scan binding `?v` as `Slot::Id`. The UNDEF probe
row merges first and takes the build side's `Id(v1)`; the second probe row keeps
its own `Term(v1)`. Each chunk is internally homogeneous — no per-chunk
normalize fires — but the stream mixes `Id(v1)` then `Term(v1)`, and a downstream
`DISTINCT ?v` counts two solutions for one logical value. A lazy/sticky
normalizer can't fix it either: once an `Id` chunk is emitted it cannot be
retracted when a `Term` arrives later.

The fix is to decide provenance **before the first chunk is emitted**, from
static information:

- New required `Op` method:

  ```rust
  /// Static per-column provenance claim, parallel to `schema()`: `true` at
  /// index `i` means column `i` MAY yield a `Slot::Term` somewhere in this
  /// op's output stream. Over-approximation; `false` is a guarantee.
  fn may_emit_term(&self) -> Vec<bool>;
  ```

  Exact per-op values: Scan `false*`; Values/CountScan/PathClosure `true*`;
  Filter/Slice/Distinct/OrderBy delegate to the child; Project/Extend remap the
  child (Extend's BIND column is `true`); Group is child-provenance on key
  columns, `true` on aggregate columns; Union and the joins take the per-column
  OR of their children. Making the method **required** (no default) turns "new
  operator forgot to declare provenance" into a compile error rather than a
  silent correctness or performance bug.

- At build time, `forced_term[c]` is computed per output column: `true` iff the
  column is **shared** (present in both child schemas) and (`left.may_emit_term`
  for it ∨ the drained build column actually contains a `Slot::Term`). One-sided
  columns pass a single stream-homogeneous source through and are never forced.
  Every emitted chunk decodes `Slot::Id → Slot::Term` in forced columns
  (`force_term_columns`, replacing the joins' `normalize_columns` call).

**Why this preserves the invariant** (inductive argument): assume both children
never mix `Id` and `Term` within a column across their whole streams (true for
every leaf; each op's rule above preserves it). Join output columns are (a)
left-only: pass-through of a homogeneous stream; (b) right-only: slots from the
homogeneous drained build side, plus `Unbound` fill (exempt); (c) shared: merged
slots come from either side, so mixing is possible **only** when a `Term` source
exists — exactly when `forced_term` fires and decodes every `Id`, making the
column all-`Term`/`Unbound`. When no `Term` source exists (`may_emit_term` false
on the left, no actual `Term` on the right — the BGP⋈BGP hot path), only
`Id`/`Unbound` slots can be emitted and **zero decode is paid**, same as today.

Cost note: `forced_term` over-approximates (it may decode a column today's
lazy "only if actually mixed" normalize would have left as `Id`), but only on
shared columns that a `Term`-capable operator feeds — and `row_join_key` already
decodes exactly those jvar columns per row for hashing, so the added decode is
bounded by work the join already does. `Slot::Id → Slot::Term` decoding is
semantically the identity at the `Bindings` boundary, so results are unchanged.

### 4. Bound-key join-variable selection

`batch_join_vars` is replaced by:

```text
bound_join_vars(left_schema, build: &Batch) -> Vec<Var>
    = { v ∈ left_schema ∩ build.schema
        | ∃ build row r: r[v] is not Unbound },   sorted by name
```

computed **after the build side is drained** — the actually-bound key, not the
schema key. Consequences:

- A shared variable unbound in every build row is dropped from the key. It
  carried zero selectivity but poisoned every row's key (`row_join_key` → `None`
  → all build rows unkeyed). With it dropped, rows key on the remaining jvars and
  hashing is restored. This is the defect fix.
- A **partially** bound shared variable stays in the key: its bound rows hash
  normally, its unbound rows go to `unkeyed` and are probed by every left row —
  which is *semantically forced*, not a defect: an unbound variable is compatible
  with any value (SPARQL §18.3), so such a row genuinely can match any probe row.
- **Empty key set** (no shared var bound anywhere, or an empty/zero-row build
  side): every row keys to `Some(vec![])` — a single bucket holding the whole
  build side, i.e. the cross-compatibility scan the semantics require. No special
  case; `merge_rows_with` still arbitrates each pair.

### Correctness w.r.t. SPARQL compatibility semantics

Two invariants make both changes result-preserving:

1. **Key selection only shrinks candidate sets, never the match set.** For any
   left row `a` and build row `b` that are compatible: for every jvar `v` in the
   key, either both sides bind `v` to the same term — same decoded lexical key
   component (`row_join_key` decodes both sides, so `Id` vs `Term` provenance
   cannot split a bucket) — or at least one side leaves `v` unbound, in which
   case that row is routed to `unkeyed` (build) or probes all rows (probe), both
   of which reach `b`. So every compatible pair is enumerated; `merge_rows_with`
   then re-checks **all** shared variables (including any dropped from the key)
   and rejects genuine conflicts. Dropping an all-unbound variable from the key
   removes a component that was identical (absent) for every potentially-matching
   pair — bucketing gets finer, candidate enumeration stays a superset of the
   match set.
2. **Chunking is invisible to the merge.** Each probe row is joined against the
   complete, immutable build state; output is the concatenation over probe rows
   in probe order. The only cross-chunk coupling — column provenance seen by
   downstream keyed operators — is handled by §3, which decides per-column
   provenance before the first emission and holds it for the whole stream.

`LeftJoin` additionally: matched/unmatched is decided per probe row against the
full build state, so OPTIONAL semantics (emit left row with unbound right vars
iff no candidate survives `merge_rows` + the inner FILTER) are chunk-independent.

## Testing & gates

- **No-change gates:** the `slot_differential` suite (runtime.rs, incl.
  `distinct_join_over_optional_no_column_mixing` and
  `inner_join_multi_row_shared_var`) and the tiny-`TEST_BATCH_ROWS`
  chunk-boundary suite (op/chunk_tests.rs `join_cross_chunk`,
  `left_join_cross_chunk`) must stay green at every step.
- **New tests:**
  - `bound_join_vars` unit tests: all-unbound shared var excluded, partially
    bound kept, empty build side ⇒ empty key set; plus an end-to-end
    `VALUES`-with-`UNDEF` build side pinning the "unbound matches anything"
    semantics across chunk sizes.
  - Streaming behavior: an instrumented counting probe child asserts the first
    `next()` pulls exactly **one** probe chunk (red against today's drain-both
    implementation), for both `JoinOp` and `LeftJoinOp`.
  - Fan-out carry: probe rows matching many build rows so one probe chunk's
    output exceeds `batch_rows()`, exercising `pending`.
  - Mixed-provenance regression (the §3 counter-example) for Join and LeftJoin:
    `DISTINCT` over a streamed join of a `VALUES`(UNDEF-first) probe against a
    BGP build must yield one row at chunk size 1. Goes red if the
    `force_term_columns` call is dropped.
  - `may_emit_term` unit tests pinning the static provenance vectors.
- **Final gate:** `cargo nextest run -p horndb-sparql` and
  `cargo nextest run -p horndb-sparql --features server` green; clippy/fmt clean.
- **Benchmark:** `agg_profile` locally as a smoke check only; official numbers
  (SPB-256 aggregation-qps) come from the **hornbench** host per root
  `CLAUDE.md`. Expectation: **net-neutral on SPB** (the mix has small probe
  sides and no all-unbound shared vars); the wins are memory/latency-shape
  (first-tuple time) and removal of the pathological O(|l|·|r|) cliff.

## Landing sequence

1. `bound_join_vars` + wire into the existing eager joins (defect fix lands
   independent of streaming).
2. `Op::may_emit_term` on all 13 operators (infrastructure, no behavior change).
3. Streaming `JoinOp`: `JoinState` + probe helpers + forced columns; delete
   `compute_join`.
4. Streaming `LeftJoinOp`; delete `compute_left_join`.
5. Gates + hornbench + doc sync (`TASKS.md` deferred items 1 and 4,
   `docs/architecture.md`, `docs/benchmarks.md` if numbers move).

## Risks

- **The stream-wide invariant is load-bearing and implicit today.** This design
  makes it explicit in the `Op` contract via `may_emit_term`; a wrong `false`
  claim in some future operator would silently break cross-chunk `DISTINCT`.
  Mitigation: the method is required (compile error to omit), documented on the
  trait, and the mixed-provenance tests pin the failure mode.
- **Probe-side all-unbound vars still degrade.** `bound_join_vars` inspects only
  the build side; a shared var unbound in every *probe* row sends each probe row
  down the probe-all path. Semantically forced per-row; an adaptive re-key is
  future work (see Non-goals).
- **Forced decode over-approximation.** A shared column fed by a `Term`-capable
  child decodes even when no mixing would materialize. Bounded (jvar-adjacent
  columns only, decode already paid for keys); watch `agg_profile` /
  hornbench for surprises.
- **Fan-out memory.** `pending` holds one probe chunk's full join output; a
  single probe row matching a huge build bucket still buffers that bucket's
  merge. Bounded by per-chunk fan-out (≪ full materialization today), inherent
  to hash join without output pagination.
