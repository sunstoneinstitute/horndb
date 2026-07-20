# `horndb-wcoj` (SPEC-03) — agent notes

Leapfrog Triejoin executor, trie iterators, planner.

- Both SPEC-03 acceptance gates are cleared (#1): the repeated-pattern
  over-production bug is fixed, and the differential fuzzer
  (`tests/differential_fuzz.rs`) runs green (256 cases, no `#[ignore]`). Run it with
  `cargo test -p horndb-wcoj --test differential_fuzz`.
- The 4-cycle benchmark (`benches/four_cycle.rs` →
  `SyntheticGraph::skewed_four_cycle`) beats the binary-hash reference on the
  canonical skewed win case.
- Magic-sets / SLG tabling remain deferred.
- **Trie-seek micro-opt gotcha:** only the depth-0 full-data level is worth a SIMD
  SoA `LevelColumn` rebuild. Rebuilding the transient SoA on every `open_level` is
  O(range) per descent and was a measured **~760× `four_cycle` regression**; deeper
  levels stay on scalar AoS `partition_point`. Re-measure `four_cycle` before
  touching the seek path.
- **SIMD intersect lives in `BatchIter`, and `active_run` must dedup.** The
  production executor (`executor/wcoj.rs::BatchIter`) has a k==2
  `horndb_simd::intersect` fast path: at prime time, if both contributing iters
  expose an `active_run` ≥ `SIMD_SEEK_MIN_RUN` (64), the pairwise intersection is
  precomputed once into `simd_buf[depth]` and drained. **Hazard:** the leapfrog
  requires *distinct* level keys, but the SoA `LevelColumn.values` keeps duplicates
  (it must, for `lower_bound_from`'s row index-mapping). So `active_run` returns the
  separate, cached `LevelColumn::distinct_run` view — feeding the raw column to
  `intersect` over-produces (a subject with N objects emits the binding N times). The
  `tests/batchiter_simd.rs` duplicate-subject test and the wide
  (`N_WIDE > 64`) `differential_fuzz` variant guard this; the narrow fuzzer (vocab 30)
  never crosses the threshold, so it does **not** cover the SIMD path on its own.
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
  gate). Marginal cost is **8.3 ns/tuple** (hornbench); the residual is the
  AoS→SoA input-column copy (`LevelColumn::from_aos` ~46%), tracked for a
  columnar `VecTripleSource` in **#239** to reach ≤5 ns.

See `INTEGRATION-NOTES.md` for design decisions.
