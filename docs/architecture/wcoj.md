# WCOJ Executor — Architecture Guide

How `horndb-wcoj` (SPEC-03) evaluates a Basic Graph Pattern with a
**Leapfrog Triejoin** — Veldhuizen's worst-case-optimal join (WCOJ) algorithm
(ICDT 2014). This guide maps the moving parts: how a BGP becomes a set of trie
iterators, how the single-variable leapfrog intersection works, how the
depth-first driver descends and backtracks over the variable ordering, and where
the SIMD intersect fast path plugs in.

Read this before touching the seek path, the descent/ascent bookkeeping, or the
SIMD fast path. For the *why* of WCOJ over binary-hash, see
`docs/specs/SPEC-03-*.md` and the 4-cycle benchmark rationale in
`benches/four_cycle.rs`. For contributor gotchas, see `AGENTS.md` (the
`CLAUDE.md` symlink) and `INTEGRATION-NOTES.md`.

## 0. Soft introduction (start here if joins are new to you)

Skip to §1 if you already know what a worst-case-optimal join is. Otherwise,
this section builds the whole idea from scratch — no prior graph-algorithms
background assumed.

### 0.1 The data: triples and a graph

HornDB stores facts as **triples** — `(subject, predicate, object)`, e.g.
`(alice, knows, bob)`. Read left-to-right: "alice knows bob." A pile of triples
*is* a graph: subjects and objects are nodes, and each predicate is a labelled
arrow from subject to object.

Internally every term (`alice`, `knows`, …) is dictionary-encoded to an integer
id (`TermId`), so the engine actually joins integers, not strings. Triples are
kept **sorted** — this matters enormously, as we'll see.

### 0.2 The query: a Basic Graph Pattern

A SPARQL query's core is a **Basic Graph Pattern (BGP)** — a set of triple
patterns where some positions are constants and some are **variables** (written
`?x`). Evaluating the BGP means: find every way to assign values to the
variables so that *all* the patterns are simultaneously present in the data.

Example — "find people X who know someone Y who in turn knows someone Z":

```
?x knows ?y .
?y knows ?z .
```

`?y` appears in both patterns, so whatever we bind it to must be consistent
across them. That shared-variable consistency requirement is a **join**.

### 0.3 The old way: join two patterns at a time

The classic approach evaluates a BGP **pairwise**. Find all `(?x, ?y)` matching
the first pattern; find all `(?y, ?z)` matching the second; then glue them
together on the shared `?y`. With more patterns you keep gluing the running
result to the next pattern, one pair at a time (a "left-deep" plan).

This works, but it has a failure mode. That intermediate `(?x, ?y)` table can be
**enormous** — far bigger than the final answer — especially for *cyclic*
queries where a constraint that would prune most of it only appears in a *later*
pattern. You pay to build a giant table, then throw almost all of it away. The
4-cycle in §1 is the textbook example.

### 0.4 The new way: bind one variable at a time, across all patterns

A **worst-case-optimal join (WCOJ)** flips the order of work. Instead of "finish
one pattern, then the next," it goes **variable by variable, but consults every
pattern at once**:

1. Pick a global order for the variables, say `?y`, then `?x`, then `?z`.
2. To bind `?y`: every pattern that mentions `?y` can offer a *sorted list* of
   the `?y`-values it allows. **Intersect** those lists — only values present in
   *all* of them can possibly work. Take the first such value.
3. Fix that `?y`, then recurse: bind the next variable `?x` under that choice,
   then `?z`, going depth-first.
4. When a variable's intersection runs dry, **backtrack** to the previous
   variable and try its next value.

Because a value is only explored if it already survives the intersection at
*every* relevant pattern, dead ends die immediately — you never build that giant
intermediate table. The total work is provably bounded by the largest the answer
could possibly be (the "AGM bound"), which is what "worst-case-optimal" means.

### 0.5 The key operation: intersecting sorted lists ("leapfrog")

Everything hinges on step 2 — intersecting several sorted lists to find their
common values, fast. The trick that names the algorithm is **leapfrog**:

> Keep a cursor in each list. Look at whichever cursor is *furthest ahead* (the
> current maximum). Jump every other cursor forward to *at least* that value
> (`seek`). If they all land on the same value, that value is in the
> intersection — emit it. Otherwise one of them overshot to a new maximum, so
> repeat. Cursors only ever move forward, so this races to the end in roughly
> one pass instead of comparing every pair of elements.

Because the lists are sorted, "seek forward to ≥ v" can skip huge stretches at
once (binary-search style), so a rare value shared by all lists is found without
scanning the common-but-useless values in between. That's the leapfrog: the
lagging cursors keep hopping over each other toward agreement.

### 0.6 Why a "trie" and what the depths mean

We don't just intersect one variable and stop — we have several variables, bound
in sequence. Sorted triples naturally form a **trie** (a prefix tree): all
triples sharing the same subject sit together; within those, all sharing the
same predicate sit together; and so on. So "fix `?y = bob`, now look at the
`?z`s available under that" is just *descending one level* into the trie under
`bob`. Binding a variable = intersecting at one trie **level**; recursing to the
next variable = **descending** a level; backtracking = **ascending**.

That's the entire algorithm: leapfrog-intersect at a level, descend, repeat;
when a level empties, ascend and advance. The rest of this guide is how HornDB
makes that fast and correct — the iterator plumbing (§3), the exact intersection
loop and its soundness invariant (§5), the depth-first driver that descends and
backtracks (§6), and a bulk-SIMD shortcut for the two-list case (§7).

## 1. Why WCOJ

A left-deep binary join evaluates a multi-pattern BGP pair by pair, and can be
forced to materialise an intermediate result far larger than the final output.
The canonical example is a cyclic query (the 4-cycle
`?a→?b→?c→?d→?a`): a binary join builds the full 3-path relation `(a,b,c,d)`
before it can apply the cycle-closing edge, even though almost every prefix
closes to the empty set.

A worst-case-optimal join never materialises an intermediate. It binds **one
variable at a time**, across *all* patterns simultaneously, and its cost is
bounded by the AGM bound (the largest possible output size) rather than by the
size of any intermediate. Leapfrog Triejoin achieves this by, at each variable,
intersecting the sorted value-sets that every pattern offers for that variable,
and recursing depth-first into the surviving bindings.

The planner (`plan.rs`, `planner.rs`) switches to WCOJ at **≥4 patterns**;
smaller BGPs and fully-ground BGPs go to the binary-hash executor
(`executor/binary_hash.rs`), which is cheaper when there is no blow-up to avoid.

## 2. The core algorithm in one paragraph

Fix a global **variable ordering** `v0, v1, …` (depth 0 = outermost). For each
pattern, build a trie iterator whose levels are that pattern's variables in
`var_order` order. To bind `v0`: take every iterator that mentions `v0`,
**leapfrog-intersect** their sorted `v0`-value streams to find the next value
`x` common to all of them. Bind `v0 = x`, descend one level in each of those
iterators (restricting their deeper levels to the sub-tries under `x`), and
recurse to bind `v1` under that restriction. When a level's intersection is
exhausted, backtrack: ascend, advance the parent past its current value, and
re-intersect. A full root-to-leaf path is one output row.

## 3. Data model: sources, ordered iterators, tries

```
TripleSource            (source/mod.rs)   — multi-ordering triple store
  └─ OrderedTripleIter  (source/mod.rs)   — sorted, depth-aware SPO cursor
       └─ PatternTrieIter (trie/source_iter.rs) — one pattern, global→local→phys
            └─ AdaptiveIter (executor/wcoj.rs)  — addresses it at *global* depth
```

**`TripleSource`** serves sorted cursors in any of the six triple orderings
(SPO, SOP, …). Its associated `Iter` type is a *generic associated type*, not a
`Box<dyn>` — deliberately, so the executor's hot path monomorphises against the
concrete iterator and the peek/seek calls inline. This makes the trait
non-object-safe; pass sources as `<S: TripleSource>` bounds, never `&dyn`.

**`OrderedTripleIter`** is the trie-shaped cursor contract
(`source/mod.rs:53`). It maintains an implicit "current path": values chosen at
upper levels constrain what is visible below.

- `peek(depth)` — next value at `depth` consistent with the current prefix (or `None`).
- `seek(depth, v)` — advance to the first value `≥ v` at `depth`.
- `open_level(depth)` — descend into the subtree under the value last peeked at `depth-1`.
- `up(depth)` — ascend one level, undoing the matching `open_level`.
- `rewind(depth)` — cheap reposition to the start of the current subtree without
  recomputing the range (used on carry-iter refresh; §6).
- `active_run(depth)` — optional contiguous sorted `&[TermId]` view for the SIMD
  fast path (§7); default `None`.

These `depth`s are **physical** trie levels (0=S, 1=P, 2=O for an SPO ordering).

### Physical vs. local vs. global depths

Three depth namespaces, translated by two adapter layers — this is the single
most important thing to hold in your head:

| Namespace | Meaning | Owner |
|---|---|---|
| **physical** | position in the source's SPO-style trie (0/1/2) | `OrderedTripleIter` |
| **local** | this pattern's *variables* in `var_order` order, `0..arity` | `PatternTrieIter` |
| **global** | the query's whole variable ordering, `0..n_vars` | `AdaptiveIter` / executor |

**`PatternTrieIter`** (`trie/source_iter.rs`) adapts one `TriplePattern` into a
variable-indexed trie. At construction it:

1. Permutes `(s,p,o)` into the chosen physical ordering (`ordering.permute`).
2. Records each physical level as either `Bound(v)` (a constant to seek+verify)
   or `Var(local_depth)`.
3. Builds `var_to_phys`: local variable-depth → physical depth, assigning local
   depths in `var_order` order so the pattern's variables nest in the same
   relative order the query eliminates them.
4. Seeks through any **leading bound** physical levels immediately, so the first
   `peek(0)` lands on the first variable. (A subject-bound pattern
   `<alice> ?p ?o` seeks S=alice at construction; local depth 0 = P, 1 = O.)

`open_level(local)` and `up(local)` fan out to the physical levels *between*
consecutive variables, seeking+verifying any bound physical level in the gap
(e.g. a pattern `?s <knows> ?o` must re-verify P=knows each time it descends
from `?s` to `?o`). This is why local→physical is not a simple offset.

**`AdaptiveIter`** (`executor/wcoj.rs:59`) is the outermost wrapper. It holds
`g_to_l[global_depth] = Some(local_depth)` for the global depths this pattern
mentions, and translates every `peek/seek/open_level/up/active_run` call from
global depth into the inner iterator's local depth. It is a concrete type (not
`Box<dyn TrieIterator>`) so *both* dispatch hops — outer `AdaptiveIter` and inner
`PatternTrieIter` → source — statically inline on the leapfrog hot path.

## 4. Planning: cutover and variable ordering

`ExecutionPlan::for_bgp` (`plan.rs:23`) makes two decisions:

- **Executor kind.** All-ground BGP → `BinaryHash` (short-circuited). Otherwise
  `≥ wcoj_cutover` (default 4) patterns → `Wcoj`, else `BinaryHash`. The
  `Planner` (`planner.rs`) is a thin wrapper holding the cutover; the
  `Cardinality` estimator is threaded through but currently unused — it is the
  seam where Stage-2 cost-based ordering will land.
- **Variable ordering.** Variables sorted by **descending degree** (how many
  patterns mention each), ties broken by first-appearance for determinism.
  High-degree-first shrinks the search space fastest: the most-constrained
  variable is intersected across the most patterns at the shallowest depth, so
  dead prefixes die early.

The chosen `var_order` *is* the trie's level structure for every pattern.

## 5. The leapfrog single-variable intersection

The heart of the algorithm. Given `k` iterators all positioned at the same
variable depth, find the next value common to all `k` (or report exhaustion).

There are **two implementations of the same algorithm**:

- `trie/leapfrog.rs` — `LeapfrogJoin`, a standalone `Box<dyn TrieIterator>`-based
  version. The readable reference; keep it in sync with the executor.
- `executor/wcoj.rs` — the same loop **inlined** against `&mut [AdaptiveIter]`
  (concrete, statically dispatched) and driven by an explicit per-depth state
  stack. This is what actually runs. `leapfrog_next` / `find_match` /
  `try_arm_simd` mirror the standalone methods.

### 5.1 The invariant

The loop only ever compares two iterators: `iter[p]` and its predecessor
`iter[prev]` where `prev = (p + k - 1) mod k`, walking `p` round-robin. For that
to be correct, the iterators must be visited in **non-decreasing key order**, so
that `iter[prev]` always holds the running *maximum* of all `k` current heads.
Then `iter[p].key == iter[prev].key` implies **all** `k` keys are equal (they're
squeezed between the min at `p` and the max at `prev`).

This ordering is established by **priming**: on the first call, peek every
contributing iterator, sort the indices by peeked key, store the permutation in
`sorted_idxs[depth]`, and start `p = 0` (the minimum).

> **Why the sort is not optional.** A priming snapshot like `[A=2, B=14, C=2]`
> compared only pairwise-in-input-order would falsely report a match of 2 — the
> loop would never discover that B holds a value the others can't reach. The
> sort is what makes "compare `p` against `prev`" equivalent to "compare against
> the global max." (See the comment at `executor/wcoj.rs:433` and
> `leapfrog.rs:69`.)

### 5.2 The convergence loop (`find_match`)

```
loop:
    prev   = (p + k - 1) mod k
    target = peek(sorted_idxs[prev])   # the running max; None ⇒ exhausted
    cur    = peek(sorted_idxs[p])      # None ⇒ exhausted
    if cur == target: return Some(cur) # all k agree — emit
    seek(sorted_idxs[p], target)       # jump the lagging iter up to the max
    if peek(sorted_idxs[p]) is None: done; return None
    p = (p + 1) mod k                  # the seeked iter is now the new max
```

Each seek makes `iter[p]` the new maximum, so rotating `p` forward preserves the
invariant. Progress is monotone (keys only ever increase), so the loop
terminates. A `seek` that returns a value `< target` would violate the `≥`
contract of `seek`; the executor treats that as exhaustion rather than looping
forever (`executor/wcoj.rs:573`).

### 5.3 Priming vs. subsequent calls (`leapfrog_next`)

- **First call (`!primed`)** — try to arm the SIMD fast path (§7); otherwise
  sort into `sorted_idxs` and run `find_match`.
- **Subsequent call** — the previous match was emitted, so advance the iterator
  that produced it (`sorted_idxs[p]`) one past its current value
  (`seek(cur.wrapping_add(1))`), then `find_match` again. If that iterator runs
  off the end, the level is exhausted.

`prime_scratch` (the `(idx, key)` sort buffer) and `sorted_idxs[depth]` are
hoisted onto `BatchIter` and reused across re-primes so their `Vec` capacity
survives the per-descent state reset — the leapfrog re-primes this depth once
per parent binding, and we don't want an allocation each time.

## 6. The depth-first driver (`BatchIter::step`)

`BatchIter` (`executor/wcoj.rs:179`) turns the per-variable leapfrog into a full
depth-first traversal producing Arrow `RecordBatch`es. The recursion is an
**explicit state stack**, not native recursion, so cancellation polling stays a
cheap top-of-loop check and the whole thing is one `Iterator`.

### 6.1 Precomputed per-depth index sets

Built once in `BatchIter::new`, all indexed by **global depth**:

- **`contributing[d]`** — iters that mention variable `d`. These are leapfrogged together at `d`.
- **`descend_at[d]`** — iters in `contributing[d]` that *also* mention a deeper
  variable. Only these need `open_level`/`up` when crossing depth `d↔d+1`.
- **`top_at[d]`** — iters whose *shallowest* (top) variable is exactly `d`.
- **`carry_at[d]`** — iters that contribute at `d` but whose top variable is
  shallower than `d`. These "carry" cursor state from a higher descent and need
  refreshing on re-entry.

Ground patterns are handled specially: any all-bound pattern is pre-checked
against the source once (`new`, the "ground-pattern pre-check"); if it doesn't
match, the whole join is empty and `finished` is set. Ground patterns get *no*
iterator — `contributing` etc. index only the non-ground patterns.

### 6.2 The descend / emit / ascend loop

`step()` loops on the state stack until it fills a batch or exhausts the join:

- **Enter a depth** (`state[d] is None`): refresh every `carry_at[d]` iter, then
  install a fresh `DepthState`.
- **Leapfrog** at `depth` via `leapfrog_next`:
  - **`Some(v)`** — bind `binding[depth] = v`.
    - If `depth+1 == n_vars` (leaf): push the full binding row into the
      `BindingBatchBuilder`; a flushed batch is returned.
    - Else (interior): call `open_level(depth)` on every `descend_at[depth]`
      iter (restricting their deeper levels to the subtree under `v`), mark
      `has_descended`, and `depth += 1`.
  - **`None`** — the level is exhausted:
    - `reset()` every `top_at[depth]` iter (undo its cursor advance so a future
      re-entry starts fresh).
    - Drop `state[depth]`. If `depth == 0`, finish and flush the final batch.
    - Otherwise `depth -= 1` and `up(depth+1)` every `descend_at[depth]` iter
      (tear the child ranges down), then loop — which re-enters the parent,
      advances it past its match (`leapfrog_next`'s subsequent-call path), and
      re-leapfrogs.

### 6.3 reset / refresh / rewind — the re-entry bookkeeping

The subtlety in the whole executor is keeping iterator cursors correct when the
same depth is entered many times under different parent bindings. Three
mechanisms, by how far the iterator's state has drifted:

- **`reset()`** (`top_at`) — a `top_at[d]` iterator's *own* leapfrog at `d`
  exhausted, so its cursor sits past the end. `reset()` rewinds it to
  post-construction state (re-seek leading bound levels, re-open to the first
  variable). Used on the exhaustion/`None` path.
- **`refresh(d)` → `rewind`** (`carry_at`) — a `carry_at[d]` iterator's cursor at
  depth `d` may have been advanced by a *deeper* leapfrog under a prior parent
  binding, but the depth-`d` *range* is still valid under the current parent (it
  was set by an ancestor's `open_level` that hasn't been torn down). So we only
  rewind the cursor to the range start (`rewind_local` → source `rewind`), no
  range recomputation. Done on entry (`state[d] is None`).
- **`up` / `open_level`** (`descend_at`) — the normal descend/ascend across a
  depth boundary, restricting or releasing the child ranges.

`AdaptiveIter::up` (`executor/wcoj.rs:120`) is careful: to ascend out of global
depth `d` it finds the most recent *shallower* global depth this iterator
actually contributed to (skipping globals it doesn't mention) and calls the
inner `up` for the physical span between them.

### 6.4 Metrics & cancellation

- `seeks` and `iterations` are plain `u64` counters accumulated on the hot path
  (no per-seek timing — SPEC metrics §5.3 forbids it) and observed once on
  `Drop` into `wcoj.seeks_per_query` / `wcoj.iterations_per_query` /
  `wcoj.peak_iterators` histograms.
- `cancel.check()` is polled once per outer loop **only at depth 0**, so
  cancellation latency is bounded by one root-to-leaf traversal without paying a
  check per seek. (Priming SIMD kernels before timing cancellation latency is
  why the test does a warmup — see commit `e609e45`.)

## 7. The SIMD `k == 2` intersect fast path

When exactly two iterators contribute at a depth and both can expose their
remaining level values as a contiguous sorted slice, the entire pairwise
intersection is computed **in one bulk SIMD call** instead of round-robin
seeking.

### 7.1 Arming (`try_arm_simd`, `executor/wcoj.rs:497`)

At prime time, if `k == 2`, ask both iters for `active_run(depth)`. If both
return `Some(slice)`, call `horndb_simd::intersect(a, b, &mut simd_buf[depth])`
once and set `simd_active`. Thereafter `find_match` just drains `simd_buf`: each
`simd_pos++` emits the next value and `seek`s **both** cursors to it, so the
child descent's `open_level` still binds the right sub-range. One seek per
*emitted* match, not per candidate — the candidate skipping was done in bulk by
`intersect`. `intersect` is symmetric, so the index reorder used to get disjoint
`split_at_mut` borrows doesn't change the output.

It falls through to the scalar leapfrog whenever `active_run` is unavailable
(`k != 2`, run shorter than the SIMD threshold, or a source with no contiguous
column).

### 7.2 The contiguous view and the dedup hazard

`active_run` is backed by `LevelColumn` (`source/soa.rs`), a transient
column-major (SoA) copy of one trie level's active `[lo, hi)` range. The dense
source stores rows as AoS `(u64,u64,u64)`; a single column over a range is
strided, which the SIMD primitives can't consume — so `LevelColumn` copies the
column out contiguously once per `open_level` and amortises it over the level's
seeks.

**The hazard, and why there are two buffers:**

- `LevelColumn.values` keeps **duplicates** — it stays 1:1 with source rows so
  `lower_bound_from` can map a slice offset back to an absolute row index for
  seeking. (A subject with N objects repeats N times at the subject level.)
- But the leapfrog intersects *distinct* level keys, and `horndb_simd::intersect`
  requires sorted, duplicate-free input. Feeding raw `values` to `intersect`
  **over-produces** — a subject with N objects would emit each binding N times.

So `active_run` returns a **separate, cached `distinct_run` view** (`soa.rs:76`):
a deduplicated copy built lazily on first use, sliced to start at the first
distinct key `≥` the cursor's current key. Two buffers, two contracts:
duplicate-preserving for seek index-mapping, deduplicated for the SIMD intersect.

> **Test coverage.** The `tests/batchiter_simd.rs` duplicate-subject test and the
> **wide** (`N_WIDE > 64`) variant of `tests/differential_fuzz.rs` guard this.
> The *narrow* fuzzer (vocab 30) never crosses the SIMD run-length threshold, so
> it does **not** exercise the SIMD path — don't assume a green narrow fuzz run
> covers the fast path.

### 7.3 Seek-path micro-opt gotcha

Only the **depth-0 full-data level** is worth a SoA `LevelColumn` rebuild.
Rebuilding the transient SoA on every `open_level` is O(range) per descent and
was a measured **~760× `four_cycle` regression**. Deeper levels stay on scalar
AoS `partition_point`. **Re-measure `four_cycle` before touching the seek path.**

## 8. Output & control flow surface

- `Executor::for_bgp` (`executor/mod.rs`) is the entry point: it plans, then
  dispatches to `Executor::Wcoj` or `Executor::BinaryHash`, both yielding
  `Result<RecordBatch>`. The `Wcoj` variant is `Box`ed because `BatchIter`
  carries the whole per-depth state stack and SIMD buffers — boxing keeps the
  enum small at the cost of one indirection per *batch*, not per tuple.
- `BindingBatchBuilder` (`batch.rs`) accumulates bound rows and flushes fixed-size
  Arrow batches; the final partial batch is emitted from the depth-0 exhaustion
  path.

## 9. Correctness invariants (don't break these)

1. **Prime-time sort.** `sorted_idxs[d]` must list contributing iters in
   non-decreasing key order before `find_match` runs, or the pairwise compare is
   unsound (§5.1).
2. **`seek` honours `≥`.** After `seek(d, t)`, `peek(d)` is `≥ t` or `None`. The
   loop's `< target` guard is a safety net, not a license to violate this.
3. **Descend/ascend symmetry.** Every `open_level` on a `descend_at` iter is
   matched by an `up` on backtrack; every `top_at` exhaustion is matched by a
   `reset`. Drift here corrupts a later re-entry's ranges.
4. **`active_run` returns distinct, sorted, cursor-relative keys** with no
   duplicates (§7.2). Raw `values` over-produces.
5. **The two leapfrog copies stay in lockstep.** Fix a bug in
   `executor/wcoj.rs`? Mirror it in `trie/leapfrog.rs` (and vice-versa).

## 10. Files & tests

| File | Role |
|---|---|
| `src/plan.rs`, `src/planner.rs` | executor choice + variable ordering |
| `src/executor/wcoj.rs` | the real inlined leapfrog + depth-first driver |
| `src/executor/binary_hash.rs` | the `<4`-pattern / ground fallback |
| `src/executor/mod.rs` | `Executor` dispatch enum |
| `src/trie/leapfrog.rs` | standalone `LeapfrogJoin` reference impl |
| `src/trie/source_iter.rs` | `PatternTrieIter` (global→local→physical) |
| `src/trie/mod.rs` | `TrieIterator` trait |
| `src/source/mod.rs` | `TripleSource` / `OrderedTripleIter` contracts |
| `src/source/soa.rs` | `LevelColumn` — SoA view + `distinct_run` |
| `src/batch.rs` | Arrow batch builder |

- `tests/differential_fuzz.rs` — 256-case differential fuzzer (WCOJ vs
  brute-force); the **wide** variant covers the SIMD path. Run:
  `cargo test -p horndb-wcoj --test differential_fuzz`.
- `tests/batchiter_simd.rs` — duplicate-subject SIMD-path guard.
- `tests/skewed_four_cycle.rs` — pins both executors against an independent
  4-cycle count.
- `benches/four_cycle.rs` — SPEC-03 acceptance #2 (≥10× win on the skewed
  4-cycle). Run benches on `hornbench`, never the laptop.

## 11. Deferred / out of scope

- Magic-sets / SLG tabling — deferred.
- Cost-based join-order and per-pattern ordering selection — the `Cardinality`
  estimator seam exists but is unused; Stage-2 territory.
- SIMD fast path beyond `k == 2` — only the pairwise case is accelerated.

## References

- Veldhuizen, *Leapfrog Triejoin: A Worst-Case Optimal Join Algorithm*, ICDT 2014.
- `docs/specs/SPEC-03-*.md` — the subsystem contract and acceptance criteria.
- `docs/research/maplib.md` — comparison with maplib's SPARQL-on-Polars execution model.
</content>
