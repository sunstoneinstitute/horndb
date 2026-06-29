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

See `INTEGRATION-NOTES.md` for design decisions.
