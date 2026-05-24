# SPEC-08 Integration Notes for `horndb-storage`

These notes describe call sites that **SPEC-02's plan** is responsible
for implementing. Nothing in this file modifies `horndb-storage`
directly; it records the contract `horndb-ml` exposes for SPEC-02
to consume.

## F5 — Provenance annotation column

`horndb-ml::provenance::MlProvenance` is the value type to store
on each inferred triple. SPEC-02 should:

1. Add an optional column `provenance: MlProvenance` to each
   predicate-partition's inferred-triples view.
2. Pack on disk via the stable discriminant bytes:
   - `MlProvenance::SYMBOLIC_TAG = 0x00`
   - `MlProvenance::ML_DERIVED_TAG = 0x01`
3. Triples written by SPEC-04 / SPEC-05 default to `Symbolic`.
4. The bulk-insert writeback from `MlRegistry::candidate_generator()`
   (called by SPEC-04 / SPEC-05) supplies `MlDerived { model, confidence }`.

The append-only discriminant rule is part of the SPEC-08 contract:
future variants must take new bytes, never reuse `0x00` or `0x01`.

## F4 — Hot-set advisor input to tiering

`horndb-ml::hotset::HotSetAdvisor::predict_hot(max)` returns
`Vec<TripleId>`. SPEC-02's tier-placement policy should:

1. Hold an `Arc<MlRegistry>` provided at construction time.
2. Periodically call `registry.hotset_advisor().predict_hot(window_size)`.
3. Bias placement toward the returned IDs **alongside** actual
   recent-access statistics (never instead of).

With `ml.enabled = false` the call returns an empty `Vec` (no-op);
tier placement therefore uses recent-access stats only — bit-identical
to a build with no advisor wired.
