---
status: approved
date: 2026-06-30
scope: "Streaming SPARQL runtime + projection/aggregate pushdown — design"
---

# Streaming SPARQL runtime + projection/aggregate pushdown — design

**Date:** 2026-06-30
**Issues:** [#143](https://github.com/sunstoneinstitute/horndb/issues/143) (streaming runtime) + [#144](https://github.com/sunstoneinstitute/horndb/issues/144) (planner pushdown), under epic [#128](https://github.com/sunstoneinstitute/horndb/issues/128).
**Builds on:** [SPEC-16](SPEC-16-id-based-slot-rows.md) (id-based slot rows) — this spec delivers the streaming + pushdown remainder that SPEC-16 deferred.
**Status:** design — pending implementation plan.

## Problem

`crates/sparql/src/exec/runtime.rs::eval` is a recursive, **fully-materializing**
`match`: every operator builds a complete `Vec<Row>` from its children before
returning a `Batch`. `run` then decodes the final batch to `Bindings` once at the
boundary. The two consequences this design removes:

- **Intermediate materialization.** Every plan node holds its entire result set in
  memory at once. A `COUNT(*)` over 400k triples allocates 400k rows just to count
  them (issue #128: 269 ms, perfectly linear in rows).
- **No pushdown.** The planner (`plan/planner.rs`) is a 1:1 lowering with no cost
  model, so projections carry unused columns and aggregates never collapse into the
  scan.

These are the two headline remaining levers for the SPB aggregation-qps gap
(HornDB ~12 qps vs GraphDB Free ~150). #143 (streaming) and #144 (pushdown) jointly
own the gap, so their designs are specified together here; #144's pushdown layers on
the streaming operator model.

## Non-goals

- Streaming results **out** to the HTTP/serialization layer. `run` keeps its
  `IntoIter<Bindings>` signature and still collects the full result set. #143 targets
  *intermediate* per-node materialization only.
- Streaming the scan leaf or WCOJ row production. The `Executor::scan_bgp_ids` seam
  keeps returning a whole `Batch`; the scan is chunked into the stream above the
  seam. The only new seam touch is the additive, read-only `count_bgp` in §3(b),
  which leans on WCOJ's existing cardinality path (cf. `Executor::cardinality_estimate`)
  rather than changing how WCOJ produces rows.
- A cost-based optimizer. Both #144 rewrites are heuristic-safe (always beneficial),
  applied unconditionally — no statistics, no cost model.
- `Group` micro-opts (sharing decoded members, dropping the `key_slots` clone) —
  that is #142, landed independently. Blocking operators reuse today's logic verbatim.
- Pushing expression evaluation onto ids/slots. Expression eval keeps running on
  transiently-decoded `Bindings` subsets (`decode_subset`), exactly as today.

## Current model (baseline)

- **`eval(&self, plan) -> Result<Batch>`** (runtime.rs:34-437): recursive `match`
  over `PhysicalPlan`; each arm fully materializes its children, then returns a
  `Batch { schema: Vec<Var>, rows: Vec<Row> }`.
- **`run(&self, plan) -> Result<IntoIter<Bindings>>`** (runtime.rs:26-30): calls
  `eval`, then `Batch::to_bindings(decode_term)` to decode `Slot::Id → Term` once.
- **Row currency.** `Row(Vec<Slot>)`, `Slot = Id(TermId) | Term(Term) | Unbound`
  (`exec/batch.rs`). Within-column homogeneity invariant (a slot index is uniformly
  `Id`/`Term`/`Unbound` across a batch) lets equality keys hash on raw ids;
  `normalize_columns` restores it after Join/Union mixing.
- **Blocking vs streaming today.** Blocking (must see all input): `Group`,
  `Distinct`, `OrderBy`, and the build sides of `Join`/`LeftJoin`/`Union`. Naturally
  streaming: `Filter`, `Project`, `Slice`, `Extend`, `Values`, `BgpScan`.
- **Expression eval** runs on `Bindings` via transient `decode_subset` (runtime.rs:441-454).
- **Gate.** The `slot_differential` proptest suite (runtime.rs:1964-2328) is the
  "no result change" guard.

## Design

### 1. Core abstraction — batch-at-a-time pull

Replace the recursive `eval` with a pull-based operator tree:

```rust
/// A pull-based physical operator. Every `Batch` it yields shares the schema
/// returned by `schema()`; `next` returns `None` at end of stream.
trait Op {
    fn schema(&self) -> &[Var];
    fn next(&mut self) -> Result<Option<Batch>>;
}
```

- **Chunks stay `Batch`.** Yielding `Batch` (schema + rows) per chunk means every
  existing row helper — `normalize_columns`, `Slot::key_part`, the merge helpers,
  `decode_subset` — is reused unchanged. The schema is re-attached per chunk; this is
  cheap (a handful of `Var`s) and keeps the differential test's `Batch` comparisons
  intact.
- **Chunk size.** A `const BATCH_ROWS: usize` (target ~4096; overridable to a tiny
  value in tests to force multi-chunk paths). Source operators slice their rows into
  `BATCH_ROWS`; transforming operators preserve/reshape; blocking operators buffer
  then emit in `BATCH_ROWS` chunks.
- **No empty chunks.** Streaming operators that may filter a whole chunk to nothing
  loop internally, pulling from their child until they have a non-empty chunk or hit
  `None`. Consumers therefore never see a `Some(empty)` mid-stream.

`run` becomes:

```rust
pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
    let plan = pushdown::rewrite(plan)?;          // §3
    let mut op = self.build(&plan)?;
    let mut all = Vec::new();
    while let Some(batch) = op.next()? {
        all.extend(batch.to_bindings(|id| self.exec.decode_term(id))?);
    }
    Ok(all.into_iter())
}
```

Decode stays exactly at the boundary. `build(&self, plan) -> Result<Box<dyn Op + '_>>`
recursively constructs the operator tree (mirrors today's `eval` match but constructs
operators instead of computing batches; lifetime ties to `&self.exec`).

### 2. Per-operator handling

| Operator | Strategy |
|---|---|
| **Scan** (leaf) | `exec.scan_bgp_ids(patterns)` once (seam unchanged), hand out `BATCH_ROWS` slices on each `next` |
| **Filter** | Pull child chunk, apply predicate per row (`decode_subset` + `eval_expr`), loop until a non-empty chunk |
| **Project** | Pull chunk, remap slots to projected schema |
| **Extend** | Pull chunk, compute new `Slot::Term` per row, append/overwrite |
| **Slice** | Offset/remaining counters carried across chunks; stops early when `length` is met |
| **Values** | Chunk the literal rows (no child) |
| **Distinct** | Persist a `HashSet<Vec<KeyPart>>` across chunks, emit only first-seen rows — **no full buffering**, only the seen-set grows |
| **Union** | Compute merged schema from child schemas up front; drain left then right, remap+`normalize_columns` each chunk into the merged schema |
| **Join** / **LeftJoin** | Build the hash table by draining the **right** (build side, as today) on first `next`; then **stream the left** chunk-by-chunk and probe. `normalize_columns` applied to outputs. `LeftJoin` emits `Slot::Unbound` for unmatched right-only columns |
| **Group** / **OrderBy** / **PathClosure** | Blocking: drain child fully, reuse today's `eval_group_native` / sort-by-keys / `eval_path_closure`, emit the result in `BATCH_ROWS` chunks |

Note `Distinct` becomes *more* streaming than today (only the seen-set is retained,
not the full input); `Join`/`LeftJoin` retain only the build side and stream the
(often larger) probe side.

### 3. #144 — two heuristic-safe rewrites (no cost model)

A `pushdown::rewrite(&PhysicalPlan) -> Result<PhysicalPlan>` pass runs before
`build`. Two transformations, each applied unconditionally because each is always
beneficial:

**(a) Column pruning / projection pushdown.** A top-down `needed_vars(node, demanded)`
analysis computes the set of variables each subtree must produce. Columns nothing
downstream consumes are pruned from scans and intermediate schemas; a `Project` whose
child already yields exactly the projected schema collapses to its child. Narrower
rows mean less per-row decode and hashing. Result-identical by construction (only
unreferenced columns are dropped).

**(b) Aggregate pushdown — the COUNT win.** Recognize
`Group { keys: [], aggregates: [Count(*) | Count(?v)] }` directly over a bare
`BgpScan` and lower it to a new physical node `CountScan { patterns, var }`. Its
operator calls an **additive** seam method:

```rust
// New default method on Executor — additive, does not change scan_bgp_ids.
fn count_bgp(&self, _patterns: &[TriplePattern]) -> Result<Option<usize>> {
    Ok(None) // backends without a fast count fall back to streaming Group
}
```

`HornBackend` implements `count_bgp` via WCOJ cardinality. `CountScanOp` yields a
single one-row `Batch` carrying the count as a `Slot::Term` literal — **never
materializing the underlying rows**. When `count_bgp` returns `None`, the rewrite is
not applied (the plan keeps the streaming `Group` over the scan), so correctness never
depends on the fast path existing.

**Scope guard (first cut).** Aggregate pushdown covers only `COUNT(*)` / `COUNT(?v)`
over a *single* `BgpScan` with **no** `DISTINCT`, **no** `GROUP BY` key, and **no**
intervening `Filter`. Filter-aware counting and multi-aggregate / grouped pushdown are
explicitly future work — the narrow case is the one issue #128 measured (269 ms
`COUNT(*)`).

### 4. Staged conversion (keep tests green throughout)

A `MaterializedOp` adapter wraps any not-yet-converted subtree: it calls the old
`eval` once and hands the resulting `Batch` out in `BATCH_ROWS` chunks, implementing
`Op`. This lets the tree be half-converted so the suite stays green at every step:

1. Introduce `Op`, `build`, the `run` loop, and `MaterializedOp`. Initially `build`
   wraps every node in `MaterializedOp` — behavior identical, infrastructure in place.
2. Convert the streaming operators (Scan, Filter, Project, Extend, Slice, Values,
   Distinct) to native `Op` impls.
3. Convert the blocking/join operators (Union, Join, LeftJoin, Group, OrderBy,
   PathClosure); delete the old `eval` and `MaterializedOp`.

### 5. Testing & gates

- **#145 lands first**, on today's materialized runtime: a deterministic
  `GROUP BY` + `COUNT(DISTINCT *)` test pinning the id-based distinct-key path
  (`KeyPart` over slot rows) directly, before the refactor churns every operator arm.
- The existing `slot_differential` proptest + #145 are the no-change gate, kept green
  at every staging step in §4.
- **New tests:**
  - Chunk-boundary correctness: run the differential cases with a tiny `BATCH_ROWS`
    so every operator is exercised across multiple chunks (esp. `Distinct`, `Slice`,
    `Join` whose state spans chunks).
  - Pushdown: `COUNT(*)` over a BGP returns the correct count **and** takes the
    `count_bgp` path (assert no per-row `decode_term`); column pruning preserves
    results bit-for-bit.
- **Final gate:** `cargo nextest run -p horndb-sparql` and
  `cargo nextest run -p horndb-sparql --features server` green; clippy/fmt clean.
- **Benchmark:** record the `agg_profile` deltas and the hornbench SPB-256
  aggregation-qps move in `docs/benchmarks.md`; sync `TASKS.md` and `docs/architecture.md`
  in the same commit (per root `CLAUDE.md` doc-sync rule).

## Landing sequence

1. #145 deterministic `GROUP BY` + `COUNT(DISTINCT *)` test (gate).
2. `Op` trait + `build` + `run` loop + `MaterializedOp` adapter.
3. Convert streaming operators.
4. Convert blocking/join operators; delete old `eval`.
5. #144 column-pruning rewrite.
6. #144 `CountScan` + `count_bgp` seam.
7. Benchmark on hornbench + docs sync (`docs/benchmarks.md`, `TASKS.md`,
   `docs/architecture.md`).

## Risks

- **Blocking operators dominate.** If most SPB aggregation queries are
  `Group`/`OrderBy`-heavy, streaming alone yields mostly a *memory* win; the
  *latency* win rides on #144 aggregate pushdown. Hence both are specified together
  and benched as a unit.
- **Chunk-spanning state.** `Distinct`/`Slice`/join build-sides carry state across
  `next` calls — the tiny-`BATCH_ROWS` tests exist specifically to flush out
  off-by-chunk bugs.
- **`count_bgp` correctness.** The fast count must match the streaming `Group` exactly
  (same triple-matching semantics); guarded by a test asserting parity between the
  pushed-down and fallback paths on the same data.
