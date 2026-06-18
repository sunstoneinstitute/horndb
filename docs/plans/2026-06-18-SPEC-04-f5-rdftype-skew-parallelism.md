# SPEC-04 F5 — `rdf:type` skew parallelism (issue #39)

Sub-issue of #4 (SPEC-04 rule completeness). Delivers SPEC-04 **F5**: partition
`rdf:type`-driven rule work by class id and parallelise across classes, so a
large skewed `rdf:type` partition no longer forces a serial scan.

## Problem

The hand-written list rules in `crates/owlrl/src/list_rules.rs` that read the
`rdf:type` partition (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`) each gather a
subject set via `store.probe(None, rdf_type, Some(class))` into an owned `Vec`,
then do per-subject filtering with `store.contains(...)` reads, serially. On a
skewed input where one class has a very large extent (the canonical LUBM
`rdf:type` blow-up), this serial per-subject loop is the cost driver.

The store reads during a materialise round are immutable (the round delta is
applied only at round end — see `engine.rs`), so the per-subject filtering is
embarrassingly parallel.

## Design

1. **Parallelism toggle.** Add `ParallelStrategy { Auto, Serial }` to
   `MaterializeOpts` (mirroring the existing `EqRepPStrategy`). `Auto` is the
   default and uses rayon above a tuned subject-count threshold; `Serial` forces
   the original sequential path and is the differential-test oracle.

2. **`Sync` bound.** rayon needs `&store` shared across threads, so the
   parallelised list-rule helpers take `&(dyn TripleStore + Sync)` and
   `materialize_with`'s store generic gains `+ Sync`. `MemStore` is `Sync`
   (`FxHashMap`/`FxHashSet` of `Copy` ids), and it is the only `TripleStore`
   impl in the workspace, so no caller breaks. The compiled `FireFn` signature
   (`fn(&dyn TripleStore, &Delta)`) is **unchanged** — parallelism is confined
   to the list-rule path.

3. **Parallelised rules.** `cls-int1`, `cls-uni`, `cax-adc`, `prp-key`. Each
   maps its subject `Vec` (or class list) with rayon's `par_iter`, producing a
   per-element `Vec<(Triple, Provenance)>`, then folds the results into the
   shared `Delta`. Below the threshold (or under `Serial`) the original loop
   runs. The `out.contains` self-dedup that the serial path uses for in-delta
   freshness is replaced by post-merge dedup in `Delta` (insert is idempotent),
   keeping output identical.

4. **Differential test** (`tests/rdf_type_skew_differential.rs`): materialise a
   set of skewed inputs with `Auto` and `Serial` and assert identical closures,
   including a proptest over random schema+instance graphs — mirrors
   `eq_rep_p_differential.rs`.

5. **Bench** (`benches/rdf_type_skew.rs`): an LUBM-shaped skewed input
   (one giant class extent feeding `cls-int1`/`cls-uni`) benched `Auto` vs
   `Serial`, registered in `Cargo.toml`. Record the number in `BENCHMARKS.md`.

## Out of scope (deferred, documented)

- Parallelising the **compiled** `cax-sco`-style rules (would require changing
  the generated `FireFn` signature) — separate Stage-2 follow-up.
- A `(p, o) → s` store index (SPEC-02) that would make `probe(None, p, Some(o))`
  O(extent) instead of O(partition); orthogonal storage work.

## Acceptance

- Parallel path (`Auto`) is the default for the four affected rules, behind a
  differential test proving identical output to `Serial`.
- A bench demonstrates the win; `BENCHMARKS.md` SPEC-04 row updated.
- `cargo fmt`/`clippy`/`test` green; W3C OWL 2 RL subset stays green.
