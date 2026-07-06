---
status: implemented
date: 2026-06-28
scope: "id-based slot rows for the SPARQL runtime"
---

# 2026-06-28 — id-based slot rows for the SPARQL runtime

> Dated design spec (SPEC-07 runtime perf). Implements the first ranked fix
> of [#128](https://github.com/sunstoneinstitute/horndb/issues/128) (HIGH —
> "SPARQL aggregation runtime: id-based bindings + hash group-by + streaming"):
> replace the string-decoded `Bindings` row with id-carrying slot rows so the
> dictionary is no longer defeated at the SPARQL boundary. Diagnostic harness:
> [`crates/sparql/examples/agg_profile.rs`](../../crates/sparql/examples/agg_profile.rs).
> Gates on the existing `cargo nextest` suite (bit-identical results) per the
> harness-first rule (SPEC-00). See `architecture.md` §9 and `docs/benchmarks.md`.

## Purpose

Cut the SPARQL aggregation throughput gap. The LDBC SPB-256 nightly serves
~13 aggregation-qps where GraphDB Free serves ~153 (~12×). The gap is
structural in the SPARQL runtime, not codegen — **PGO is the wrong lever**.
This spec proposes the first and largest fix: stop decoding `TermId → String`
for every result row before aggregation runs. The deferred remainder of #128
(streaming, projection/aggregate pushdown) is named explicitly as out of scope.

## The measured problem

Confirmed firsthand in-process against the real `HornBackend` via the
diagnostic harness `crates/sparql/examples/agg_profile.rs` (synthesises an
SPB-ish "creative works" graph and times representative aggregation queries):

- `Q1 COUNT(*)` over 400 k triples takes **133 ms/q**, but `len()` (the same
  count without materialising any row) takes **≈ 0 ns**. The entire query is
  `TermId → String` decode.
- The cost is **linear at ~333 ns/row**: 100 k → 32 ms, 200 k → 67 ms,
  400 k → 133 ms, 800 k → 272 ms. A scalar answer (`COUNT(*)` returns one
  number) that scales O(rows) is the signature of per-row decode.

**Root cause — the dictionary is defeated at the SPARQL boundary.**
`HornBackend::scan_bgp` (`crates/sparql/src/exec/horn.rs:597-607`) takes the
WCOJ executor's `UInt64Array` `TermId` columns and decodes every cell back to
a heap `Term::Iri(String)` via `dict.lookup` *before* any operator runs. The
row type `Bindings = BTreeMap<String, Term>` (`crates/sparql/src/exec/mod.rs:15-22`)
then holds owned decoded strings, keyed by variable name, for every row at
every node. So `COUNT(*)` spends its whole budget allocating ~1.2 M strings
(400 k rows × 3 columns) to count what `len()` already knows.

The recent O(n) `DISTINCT` dedup fix
([b185f3f](https://github.com/sunstoneinstitute/horndb/commit/b185f3f),
#128) optimised a **non-dominant** cost — which is exactly why the nightly
aggregation number did not move. The dominant cost is the decode tax above.

## Proposed design

Run the runtime on dictionary ids and decode to strings exactly once, at the
result boundary. Equality-based operators (the aggregation hot path) then
never touch a string; value-needing operators decode on demand.

### 1. Core types (new, in `exec/`)

```rust
/// One cell of a solution row.
enum Slot {
    Id(TermId),   // from scan — the hot, common case (no string)
    Term(Term),   // computed: BIND/aggregate output, VALUES literal, path-synthesised
    Unbound,      // OPTIONAL right-side with no match
}

struct Row(Vec<Slot>);

/// A block of rows sharing one schema. `schema[i]` names slot `i`.
struct Batch { schema: Vec<Var>, rows: Vec<Row> }
```

The schema maps slot index ↔ variable, replacing the per-row
`BTreeMap<String, _>` keying. Strings appear before the boundary only through
one seam:

```rust
trait Resolver {
    fn decode(&self, id: TermId) -> Result<Term>;
}
```

Backed by the `HornBackend` dictionary, `decode(id)` is exactly today's
`oxrdf_to_algebra(dict.lookup(id))`, so a decoded `Slot::Id` is **bit-identical**
to the `Term` the current scan produces. `MemStore` (the in-process test
double, which has no dictionary and erases term kinds on scan) emits
`Slot::Term` directly and pairs with an **identity resolver**, so the *same*
slot code path runs under test — just without the id speedup.

### 2. Decode boundary (public API unchanged)

`Runtime::eval` returns a `Batch`. `execute_query_with`
(`crates/sparql/src/api.rs`) calls `batch.decode(resolver)` → `Vec<Bindings>`
**once**, immediately before building `QueryAnswer::Solutions`. `QueryAnswer`,
all four result serializers (`results/{json,xml,csv,tsv}.rs`), and the axum
HTTP server are untouched. Producing bit-identical `Bindings` at the boundary
is the no-regression proof: every existing snapshot and result-format test
must pass byte-for-byte.

### 3. What runs on raw ids, what decodes

The dictionary assigns one id per canonical term — distinct lexical forms keep
distinct ids (verified by `non_canonical_integer_keeps_distinct_identity` in
`crates/storage/src/dictionary.rs`). SPARQL `GROUP BY`, `DISTINCT`, and join
compatibility are defined by RDF **term identity**, which is exactly id
equality. So these run on raw ids with **zero decode**:

- `COUNT(*)` = `rows.len()`;
- `GROUP BY` key = `&[Slot]` compared by id;
- `DISTINCT` = hash of the `Row`;
- join compatibility = shared-slot id equality;
- diagonal filters (repeated BGP var, `horn.rs:610`) = column id equality.

**Value-needing operators decode on demand:** `FILTER` comparisons, arithmetic
/ `BIND`, `ORDER BY`, and `MIN`/`MAX`/`SUM`/`AVG`/`GROUP_CONCAT`. The seam
reuses the existing ~600-line expression layer (`eval_expr` / `eval_func` in
`runtime.rs`) **verbatim**: for an expression node, decode only the slots the
expression references into a transient `Bindings`, then call the current
evaluator unchanged. Inline integers (`try_inline_int`) carry their value *in*
the id, so `SUM(?v)` over inline-int literals does **zero** dict hits.

**Explicit rule — ids are not value-ordered.** Ids are assigned in insertion
order, not value order. Therefore `ORDER BY`, `MIN`, `MAX`, and the relational
comparisons (`<`, `>`, `<=`, `>=`) **always decode** their operands; only
equality / identity may shortcut on ids.

**Slot comparison rule.** Comparing two slots for equality: `Id == Id` →
compare ids directly; any other mix (`Id` vs `Term`, `Term` vs `Term`,
involving `Unbound`) → decode both sides and compare by term. This keeps
correctness when a computed `Slot::Term` (e.g. a `BIND` result) meets a scanned
`Slot::Id` in a join key or `GROUP BY`.

### 4. Executor trait

Add a new method returning ids, leaving the decoding scan in place for callers
not yet ported:

```rust
fn scan_bgp_ids(&self, patterns: &[TriplePattern]) -> Result<Batch>;
```

`HornBackend::scan_bgp_ids` reads the WCOJ `UInt64Array` columns straight into
`Slot::Id` cells — i.e. it drops the `dict.lookup` decode loop
(`horn.rs:600-604`) out of the hot path. The existing string-returning
`scan_bgp` is retained for the not-yet-ported `DESCRIBE` path and for
`MemStore`.

### 5. Incremental rollout (the tracer bullet)

`Batch` carries `from_bindings` / `to_bindings` adapters so any operator can be
ported as "decode → run today's code → re-encode" **before** it is rewritten to
native slots. This keeps every slice independently green. The runtime has 13
operators: `BgpScan`, `Join`, `LeftJoin`, `Filter`, `Union`, `Project`,
`Distinct`, `Slice`, `OrderBy`, `Extend`, `Values`, `Group`, `PathClosure`.

- **Slice 1 — the measurable win.** Add the types + `Resolver` + native
  `scan_bgp_ids`; switch `Runtime::eval` to return `Batch`; port **Join,
  Group, Distinct, Project, Filter** to native slots. `Slice` (`LIMIT`/`OFFSET`)
  comes along for free — it is purely row-structural (skip/take, no cell
  access), so it operates on a `Batch` natively with no decode and no schema
  change. Route the remaining six — **LeftJoin, Union, OrderBy, Extend, Values,
  PathClosure** — through the decode-adapter (correct, not yet fast). Decode at
  the boundary. The full `cargo nextest` suite stays green; `agg_profile` Q1–Q5
  and the SPB nightly show the jump.
- **Slice 2.** Port the remaining six operators to native slots; remove the
  decode-adapter.

**Deferred — out of scope here, remain open under #128:**

- Streaming / no full per-node `Vec` materialisation (every node still
  buffers a whole `Batch`). Tracked alongside the deferred F6 streaming row in
  `architecture.md` §9.
- Projection & aggregate **pushdown** in the planner (the planner stays a 1:1
  lowering; the scan still materialises every projected column).

These compound with this change but are independent; deferring them keeps the
slice reviewable.

### 6. Correctness invariants

- id-equality ⇔ term identity — applies to **equality** operators only.
- `ORDER BY` / `MIN` / `MAX` / relational comparisons **always** decode (ids
  are not value-ordered).
- Computed, `VALUES`, and unbound cells are carried as `Slot::Term` /
  `Slot::Unbound`, never as a fabricated id.
- Boundary decode reproduces today's `Term` variants exactly. MemStore
  kind-erasure stays valid because the reused expression layer still receives
  decoded `Term`s exactly as it does today.

## Risks and tradeoffs

- **Single-type runtime is all-or-nothing for `eval`'s return type.** Switching
  `Runtime::eval` from `Vec<Bindings>` to `Batch` touches every operator arm at
  once. The `from_bindings` / `to_bindings` adapter is the mitigation: an
  operator can ride the adapter (decode → old code → re-encode) and still
  compile and pass tests before it is ported to native slots, so the change
  lands in reviewable slices rather than one big-bang rewrite.
- **Schema bookkeeping is the new surface.** `Project` reorders/selects slots,
  `Join`/`Union` must align or union child schemas, and `LeftJoin` fills
  unmatched right-side slots with `Unbound`. This index-based plumbing replaces
  variable-name lookups and is where porting bugs will concentrate — it is the
  focus of the slot-comparison unit test and the differential property test.
- **Computed-term equality.** A `Slot::Term` meeting a `Slot::Id` in a join key
  or group key cannot shortcut on ids; the slot comparison rule decodes both.
  The common all-`Id` path stays fast; only mixed keys pay the decode.
- **No correctness drift.** Bit-identical boundary output is the backstop. The
  property test (slot path ≡ legacy path on random `BGP` + `GROUP BY`) catches
  any schema/slot bug that the snapshot tests miss.

## Acceptance criteria

Harness-first: this fix is not satisfied until the suite stays green **and** the
aggregation bench moves.

1. **The existing `cargo nextest run --workspace` suite stays green**, with
   result-format and snapshot tests **byte-identical** to before. Non-negotiable
   — this is the no-regression proof that boundary decode reproduces today's
   `Bindings`.
2. **New unit test — slot comparison.** Exercises the `Id == Id`,
   `Id` vs `Term`, `Term` vs `Term`, and `Unbound` mixes against the rule in
   §3.
3. **New property test — differential equivalence.** Over random `BGP` +
   `GROUP BY` (+ `DISTINCT`) queries, the slot-path decoded output equals a
   reference oracle. To make the comparison runnable after `eval` switches to
   `Batch`, retain the legacy string runtime as a test-only function (e.g.
   `eval_legacy` behind `cfg(test)`) for the duration of the migration; the
   property test runs both and asserts equality, and the legacy function is
   deleted when Slice 2 lands. Backstops silent schema/slot bugs the snapshot
   tests miss.
4. **`agg_profile` before/after on hornbench.** `Q1 COUNT(*)` drops from
   ~333 ns/row toward `len()`-bound; record Q1–Q5 per/q and qps. Run on
   `hornbench` (stable env), never the laptop.
5. **Nightly `aggregation-qps` moves materially off ~13** on the SPB-256
   nightly. Record the new HornDB number (and the GraphDB A/B baseline) in
   `docs/benchmarks.md`. The 12× gap will **not** fully close here — the deferred
   streaming/pushdown work owns the remainder; say so in the bench note.
6. **Docs sync (during implementation, not in the spec commit):** check off the
   #128 sub-scope in `TASKS.md`, flip the matching `architecture.md` §9 rows
   (the aggregation "implemented (correct, slow)" and the `scan_bgp` perf
   limitation note) to reflect id-based bindings, and mirror to the GitHub
   issue per the `TASKS.md` header procedure.

## Staging note

Ship **Slice 1 first** — it is the tracer bullet that proves the seam end to
end (native `scan_bgp_ids` → slot Join/Group/Distinct/Project/Filter → boundary
decode) and delivers the measurable nightly jump, with the other six operators
correct-but-adapter-backed so nothing regresses. Land it and record the
`agg_profile` + nightly numbers in `docs/benchmarks.md` against the pre-change
baseline. **Then Slice 2** (port the remaining operators, drop the adapter),
measuring against the Slice-1 baseline so attribution stays honest. The
streaming and pushdown follow-ups stay under #128 and are picked up only after
this lands and its bench is recorded.
