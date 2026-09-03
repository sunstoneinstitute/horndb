# `horndb-wcoj` (SPEC-03) — agent notes

Leapfrog Triejoin executor, trie iterators, planner.

- Both SPEC-03 acceptance gates are cleared (#1): the repeated-pattern
  over-production bug is fixed, and the differential fuzzer
  (`tests/differential_fuzz.rs`) runs green (256 cases, no `#[ignore]`). Run it with
  `cargo test -p horndb-wcoj --test differential_fuzz`.
- The 4-cycle benchmark (`benches/four_cycle.rs` →
  `SyntheticGraph::skewed_four_cycle`) beats the binary-hash reference on the
  canonical skewed win case.
- **Join planning is cost-based (HDB-46, SPEC-23 §5.5).** `Planner::choose(bgp,
  &dyn Stats)` returns a `JoinSpec` tree (`plan.rs`): GYO cyclic core → one WCOJ
  node, never split; `CostModel` (`cost.rs`) prices WCOJ extensions by i-cost and
  hash joins by build+probe; DP over connected pattern subsets (≤ 10 patterns,
  100k-visit budget, else greedy). `ZeroStats` (`is_informed() == false`) skips
  the search: one WCOJ node in degree order. `HORNDB_WCOJ_CUTOVER=<n>` restores
  the retired fixed cutover for bisection. The fuzzer runs the planned and a
  hand-built hybrid spec against the oracle on every case;
  `tests/planner_choice.rs` pins the structural rules and the HDB-108 q3
  variable order. `HASH_BUILD_WEIGHT` / `MATERIALIZE_WEIGHT` are uncalibrated
  knobs — tune on hornbench, never by hand on the laptop.
- Magic-sets / SLG tabling remain deferred.
- **`VecTripleSource` is columnar (#239).** Each ordering is three `Vec<TermId>`,
  one per trie level, so a level's values are contiguous and the SIMD primitives
  read them in place (SPEC-03 NF2). The old row-major layout needed a transient
  per-level copy (`LevelColumn`, in the since-deleted `source/soa.rs`), and
  rebuilding that copy on
  every `open_level` was O(range) per descent — a measured **~760× `four_cycle`
  regression**. No column is built now, so `seek` can take the SIMD `lower_bound`
  path at every depth. **Still re-measure `four_cycle` before touching the seek
  path.**
- **`VecTripleSource` builds orderings on first use (HDB-97).**
  `from_triples` materialises exactly one ordering — the **anchor**,
  `ANCHOR_ORDERING` — and the other five are derived from it the first time
  something asks for them (`iter`, `sorted_columns`). A whole trainmarks run
  touches three of the six, and `q6`'s cold run touches one, so eager
  six-ordering construction was building five indexes for nobody: at 10M
  triples one ordering is ~240 MB and one sort pass.
  The anchor is **`Pso`** because `horndb-storage`'s snapshot scan already
  yields `(predicate, subject, object)` order (predicate-major,
  subject-major), so building it from a store snapshot is a linear pass, not
  a sort. Any other input order costs one ordinary sort.
  Two consequences to keep in mind:
  - The build now happens **inside** the first `iter(ord)` call rather than
    before the executor starts. In production `HornBackend::wcoj_snapshot`
    still runs it off the cancellable path, but a test that clocks executor
    latency must prime the orderings first — `tests/cancel.rs` does, next to
    its SIMD prime.
  - `supports(ord)` is true for all six and `iter` never returns
    `OrderingUnavailable`: every ordering is derivable.
  - Deriving an ordering that **shares the anchor's level-0 axis** (`Pos`
    from `Pso` — both predicate-major) sorts each level-0 block on its own
    rather than all n rows, since no row crosses a block boundary:
    `TripleColumns::derive_blockwise`, O(n log(n/b)) for b blocks (HDB-98).
    The anchor is deduplicated, so a block's rows are already distinct as a
    pair and the derive needs no dedup pass. Any other ordering (`Spo`,
    `Sop`, `Osp`, `Ops` from `Pso`) still costs a global sort.
- **`VecTripleSource` supports in-place delta maintenance (HDB-82).**
  `apply_delta(dels, adds)` merges a batch of retracted and inserted triples
  into the anchor and into every other ordering already materialised, leaving
  each sorted and deduplicated — the same state `from_triples` produces. Cost
  is O(n + k log k) per materialised ordering for a delta of k rows against a
  base of n, against O(n log n) for a rebuild. An ordering built *after* a
  delta derives from the already-updated anchor, so the two paths agree.
  `horndb-sparql`'s `HornBackend` uses it to keep its memoised snapshot warm
  across a small `SPARQL Update` instead of re-indexing the whole store; that
  caller falls back to a full rebuild whenever the merge is not provably
  correct or not profitable. If you change the sorted-and-deduplicated
  invariant, leapfrog correctness breaks — `apply_delta_matches_full_rebuild`
  is the guard.
- **`VecTripleSource` is `Clone` (HDB-97).** An `O(n)` deep copy of whatever
  orderings the source has materialised, against a `from_triples` rebuild.
  `horndb-sparql`'s `HornBackend` uses it to clone its memoised
  `DefaultStrict` snapshot into `DefaultUnion` (or vice versa) when a store's
  graph shape makes the two read the same triples, instead of paying a second
  build for an identical source.
- **SIMD intersect lives in `BatchIter`, and `active_run` must dedup.** The
  production executor (`executor/wcoj.rs::BatchIter`) has a k==2
  `horndb_simd::intersect` fast path: at prime time, if both contributing iters
  expose an `active_run` ≥ `SIMD_INTERSECT_MIN_RUN` (64), the pairwise
  intersection is precomputed once into `simd_buf[depth]` and drained.
  **Hazard:** the leapfrog requires *distinct* level keys, but at depths 0 and 1
  the stored column repeats a key once per child row. So `active_run` returns a
  cached deduplicated copy for those depths — feeding the raw column to
  `intersect` over-produces (a subject with N objects emits the binding N times).
  The leaf (depth 2) needs no dedup: under a fixed `(level0, level1)` prefix the
  object column of deduplicated triples is already strictly increasing, so it is
  returned as a slice with no copy. The `tests/batchiter_simd.rs`
  duplicate-subject test and the wide (`N_WIDE > 64`) `differential_fuzz` variant
  guard this; the narrow fuzzer (vocab 30) never crosses the threshold, so it
  does **not** cover the SIMD path on its own.
- **Per-tuple hot path (SPEC-03 NF1, #237).** The leapfrog descent (`VecIter`)
  finds child-run boundaries (`open_level`) and repositions cursors (`seek`) with
  a **bounded gallop from the cursor** (`run_end` / `seek_gallop`), not a
  `partition_point`/`lower_bound` bisect of the whole parent range — the common
  narrow-run-under-wide-parent shape was ~log(range) cache-missing probes to
  advance a few rows. Both return bit-identical lower bounds; a far `seek` target
  bails to the exact same binary search, so SPB-style varied far seeks are
  unaffected. Guarded by `run_end`/`seek_gallop` oracle unit tests + the fuzzer.
- **Armed leaf is bulk-materialized, not drained per value.** At the final
  variable an armed `k==2` leapfrog has the whole binding set in `simd_buf`;
  `step` blits it into the batch via `BindingBatchBuilder::push_run_chunk`
  (ancestor binding replicated across prefix columns, intersection into the leaf
  column), bypassing the per-value `find_match`/`push_row` machinery. A
  `simd_tried` flag stops the leaf pre-arm and the scalar prime from both probing
  `active_run`. `benches/per_tuple.rs` has two cases: `two_star_50k`
  (descent-bound, will not hit NF1) and `wide_4x100k` (marginal hot path, the NF1
  gate). Marginal cost was **8.5 ns/tuple** (hornbench) with the residual in the
  row→column input copy; #239 removed that copy by making the source columnar,
  taking it to **2.74 ns/tuple** — **NF1 (≤5 ns) is met**. Same-session A/B, so
  the no-regress side is on the record too: `two_star_50k` 56.1 → 49.0 ns/tuple,
  `four_cycle/wcoj` 195.8 → 173.9 ms (`docs/benchmarks.md`).

See `INTEGRATION-NOTES.md` for design decisions.
