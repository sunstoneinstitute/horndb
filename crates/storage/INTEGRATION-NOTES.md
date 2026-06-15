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

## Snapshot format (SPEC-02 F9)

`snapshot/` exports the default graph of a `Store` to a compact byte
stream and re-imports it (`Store::export_snapshot` / `import_snapshot`,
free fns `export_snapshot` / `import_snapshot`, accounting via
`SnapshotStats`). Design decisions that aren't in the spec:

- **HDT-*derived*, not rdfhdt wire-compatible.** The three-section layout
  mirrors HDT (Header / Dictionary / Triples) but is our own encoding.
  Cross-tool interop with rdfhdt and friends is an explicit non-goal of
  this increment — do not assume a `.hdt` produced elsewhere will load.
- **Default graph only.** Export *errors* if the store holds named-graph
  data (`has_named_graph_data` guard) rather than silently dropping it.
  Named-graph / quad snapshots are a documented follow-up.
- **Operates at the `oxrdf::Term` level**, not the internal `TermId`
  level. This makes the format robust to dictionary id reassignment:
  the dictionary stores terms by their labels, so a round-trip is
  label-preserving and reduces to exact triple-set equality (which
  trivially satisfies acceptance #5's "isomorphic under blank-node
  renaming").
- **Three sections:** a 32-byte fixed header; a dictionary of distinct
  terms sorted by a canonical kind-tagged byte encoding and front-coded
  (shared-prefix elision exploits common IRI prefixes); and an SPO
  adjacency list over dense local ids, gap-coded with VByte (LEB128).
  Inline-int terms (`TermKind::InlineInt`) get a compact value-encoded
  dictionary entry so int-heavy data stays small.
- **Measured footprint: 5.440 B/triple** on a 40k-triple LUBM-shaped
  synthetic corpus (NF1 budget is ≤6 B/triple). Caveat: the triples
  section dominates and per-id VByte width grows with the id space, so
  this is *synthetic* — validate against a real LUBM corpus before
  treating NF1 as comfortably banked.

Full byte-level layout and the canonical term encoding are specified in
`docs/plans/2026-06-14-SPEC-02-hdt-snapshot.md` (see its "Format
specification" section).

## Copy-on-write snapshot isolation (SPEC-02 #19, delivered)

`MemoryTier` holds an immutable, versioned `Arc<TierSnapshot>` behind
`RwLock<Arc<…>>` plus a writer `Mutex`. `insert_quad_batch` is copy-on-write:
it clones the top-level graph map (Arc clones of untouched graphs), rebuilds
only the affected graphs' partition maps, bumps the version, and atomically
swaps the live pointer. `Store::snapshot()` / `StoreSnapshot` pin a stable,
internally-consistent read view; concurrent writers never disturb a pinned
snapshot, which stays readable until dropped. The dictionary is append-only, so
pinned term ids never change meaning. HDT export reads one pinned snapshot, so a
checkpoint taken under concurrent writes is internally consistent (NF5). True
per-tuple-visibility MVCC remains deferred to Stage 2 (SPEC-06).
