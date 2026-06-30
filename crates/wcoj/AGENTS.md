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

See `INTEGRATION-NOTES.md` for design decisions.
