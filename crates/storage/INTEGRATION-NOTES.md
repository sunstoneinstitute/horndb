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
`docs/plans/PLAN-02-02-hdt-snapshot.md` (see its "Format
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
checkpoint taken under concurrent writes is internally consistent (NF5).
Per-tuple visibility (row-level delete) is the next section, delivered under
`SPEC-25` S1.

## Partition runs and deferred merge (HDB-84, delivered)

A `PredicatePartition` holds a list of sorted **runs** — blocks of rows whose
concatenation is the partition — plus a `OnceLock` cache of the merged view.
`insert_quad_batch` appends one run per batch: it sorts only that batch and
shares the rows already stored by `Arc`. Every read path (`subjects()`,
`ordered_at()`, `live_len()`, `stats()`, and the rest) goes through the merged
view, which is built on first read by the same sort-and-dedup a single-shot
build always used — so the columns, side-sets, visibility stamps and live count
are identical however the rows arrived.

What this changes for callers:

- Repeated small writes cost the rows they carry plus the run list they clone,
  not the rows already stored. Before, N batches into one predicate paid
  O(existing) N times.
- The first read after a batched write is O(rows in that partition), once. Any
  read pays it; `MemoryTier::stats()`, `HornBackend::storage_stats()` and a
  Prometheus scrape all count as reads here, so none of them is strictly
  O(partitions) any more. It emits a `merge_runs` load phase.
- `retract_quad_batch`, `apply_quad_batch` and `compact()` still rebuild a
  partition row by row — they have to touch every row anyway — so they force
  the merge first.

**A read can now stall a write.** The merging thread is a reader. It holds no
writer lock, and it holds the partition's `runs` mutex for the whole merge — a
sort of every row, plus a second sort for the object-major layout above
`hot_threshold`. A concurrent `with_appended_rows` on that same partition waits
it out: on a 10M-row predicate, order of seconds. The work is not new (the
pre-HDB-84 tier charged the same sort to the writer, on *every* batch) and it
runs once per partition version, but the direction is: reader-blocks-writer did
not exist before. `MemoryTier`'s writer mutex does not bound it, because the
merging thread never takes that mutex.

**Run count is capped** at `partition::MAX_RUNS` (4,096). Two costs grow with
it — a write clones the run list, and each run carries ~1 KiB of fixed Arrow +
Roaring overhead however few rows it holds — so on reaching the cap the write
merges instead of appending. A batched bulk load does not get near it (10M
triples in 8,192-triple batches is 1,221 runs). The pattern that does is
`Store::insert_triples` called one triple at a time with no read in between:
that caller now pays a full O(rows) merge every 4,096 inserts, against the
pre-HDB-84 tier's merge on *every* insert. Bounded, and strictly better than
before, but single-triple insert into a columnar partition is still the wrong
shape — batch it.

The bulk loaders' batch size is `HORNDB_LOAD_BATCH_TRIPLES`
(`loader::load_batch_triples`, default 65,536). It is a memory knob now, not an
index-rebuild knob: measured load cost is flat in it (`docs/benchmarks.md`).

## Per-tuple MVCC (SPEC-25 S1, delivered)

Substrate: two stamp columns, `begin`/`end` (`CommitVersion = u64`, `visibility.rs`),
added to each `PredicatePartition` alongside the `(subject, object)` columns —
not a delete-bitmap sidecar and not in-place append. A row is visible at
version `v` iff `begin <= v < end`; `end == UNSET_END` (`u64::MAX`) means live.
Insert stamps `begin = commit_version, end = UNSET_END`; retract stamps
`end = commit_version` on the matching live row — the row stays physically
present, a delete is a stamp, not an eviction. This keeps the existing
copy-on-write substrate (immutable `Arc<TierSnapshot>` swapped per commit)
unchanged; MVCC is layered on top of it, not a replacement for it. The
hornbench comparison against delete-bitmap sidecars and in-place append,
against the NF4 write-amplification budget, is deferred — [#242](https://github.com/sunstoneinstitute/horndb/issues/242).

- **`Tier::retract_quad_batch(&[(GraphId, TermId, TermId, TermId)]) -> Result<usize>`**
  (`memory_tier.rs`, `tier.rs`): one call = one commit version. A quad absent
  from the current live set is a **counted no-op** — it does not bump `end` on
  anything and does not error — so retracting an already-absent quad is safe
  and idempotent. The returned count is how many quads actually matched a live
  row.
- **Read filter:** every read helper is version-parameterized —
  `scan_at`/`ordered_at`/`subject_set_at`/`object_set_at`/`len_at` on
  `PredicatePartition` (`partition.rs`) — and applies `begin <= at < end`.
  **Zero-copy fast path:** when a partition has no retracted rows at all
  (`!has_retractions()`) and the query version is at or after the partition's
  newest insert (`at >= max_begin`), the filter is skipped entirely and the
  raw columns are returned as-is. This is the common insert-only case, so the
  WCOJ hot-path benches do not regress from the MVCC read filter.
- **Compaction + pin registry:** `MemoryTier`/`Store::compact()` builds a fresh
  partition dropping rows whose `end <= min_pinned_version`; it never mutates a
  row a pinned view still needs (pinned `StoreSnapshot`s hold their own older
  `Arc<TierSnapshot>`). The pin registry (`Mutex<BTreeMap<u64, usize>>`,
  version -> live pin count) tracks the oldest version any snapshot still
  holds. **Compaction is explicit-only today** — nothing calls `compact()`
  automatically, so dead (retracted) rows accumulate under insert/retract
  churn until a caller invokes it. A compaction trigger policy is part of the
  deferred hornbench follow-up ([#242](https://github.com/sunstoneinstitute/horndb/issues/242)).
- **SPEC-24 S6 surface** on `StoreSnapshot` (`store.rs`), still default-graph
  scoped: `contains(s, p, o)`, `iter_all_term_ids()` (ordered), and
  `logical_time()` (== the pinned commit version, ADR-0018's clock binding).
  This is the storage-side half of the SPEC-24 S6 contract; wiring
  `horndb-incremental`'s `Circuit::snapshot()` onto it is separate, tracked
  under [#215](https://github.com/sunstoneinstitute/horndb/issues/215).
  `len()`/`is_empty()` are **not** part of this list any more — SPEC-28 S2
  ([#265](https://github.com/sunstoneinstitute/horndb/issues/265)) flipped
  them to whole-store. The graph-scoped surface an S6 backing should target
  instead is `graph_len(GraphId)` and `iter_graph_term_ids(GraphId)`
  (key-ordered) — see `docs/specs/SPEC-28-named-graph-dataset-semantics.md`
  §S2.
- **`horndb-sparql` overlay retired:** `HornEngine`'s `tombstones: HashSet`
  is gone; `DELETE DATA` and pattern delete now call `Store::retract_*`
  directly and reads see the store's own visibility filter.

## SPEC-28 S6 — idempotent quad-grain apply (delivered)

`Tier::apply_quad_batch(dels, adds) -> Result<ApplyReport>` (`tier.rs`,
`memory_tier.rs`) is the store-boundary primitive S6 requires: one commit
version covers a whole dels-then-adds batch (dels apply first, so a
delete+insert of the same quad in one batch ends present), `ApplyReport {
retracted, inserted }` reports only *actually-changed* counts, and a batch
whose net effect is empty does not bump the version — extending the SPEC-25
S1 retract no-op rule to the combined path. `Store::apply_quads` is the
`Term`-level counterpart: deletion terms are looked up, not interned
(mirroring `Store::retract_quads` — a term never seen retracts nothing);
insertion terms are interned.

`Store::insert_quads` / `retract_quads` are thin wrappers (`apply_quads(&[],
q)` / `apply_quads(q, &[])`) and **keep their pre-existing `Result<usize>`
signatures** rather than moving to `Result<ApplyReport>` — `retract_quads`
already returned a count before SPEC-28 and existing call sites destructure
it directly; `insert_quads` gains the matching shape (`Result<()>` →
`Result<usize>`) instead of a wider breaking change across both.
`Tier::insert_quad_batch` / `retract_quad_batch` are untouched:
`Store::insert_triples` / `retract_triples` (default-graph, triple-grain)
still call them directly, so that path keeps its older "insert always bumps
the version" behaviour — only writes that go through `apply_quads` get the
empty-batch-no-bump guarantee.
