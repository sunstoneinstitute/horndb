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

`snapshot/` exports every graph of a `Store` — the default graph plus
all named graphs — to a compact byte stream and re-imports it (`Store::export_snapshot` / `import_snapshot`,
free fns `export_snapshot` / `import_snapshot`, accounting via
`SnapshotStats`). Design decisions that aren't in the spec:

- **HDT-*derived*, not rdfhdt wire-compatible.** The three-section layout
  mirrors HDT (Header / Dictionary / Triples) but is our own encoding.
  Cross-tool interop with rdfhdt and friends is an explicit non-goal of
  this increment — do not assume a `.hdt` produced elsewhere will load.
- **Quads, with a version gate** (SPEC-25 S4). Export covers every graph
  the pinned snapshot enumerates, and a round trip is exact *quad*-set
  equality. Two format versions exist: v1 is the Stage-1 layout (one
  default-graph adjacency block) and is still what a store with no
  named-graph data writes; v2 replaces that single block with a graphs
  section. A Stage-1 reader accepts only v1, so it rejects a v2 snapshot
  through the existing `unsupported snapshot version` path instead of
  misreading it. `format::read_snapshot_upto` makes that ceiling explicit
  and is how the compatibility gate is tested.
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
  dictionary entry so int-heavy data stays small. In v2 the third
  section is instead `num_graphs` followed, per graph, by
  `graph_local` (a VByte local id: 0 for the default graph, otherwise
  the graph name's dictionary local id), the graph's triple count, and
  that graph's adjacency list — same encoding, applied per graph.
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
snapshot, which stays readable until dropped. Interning only ever appends and
no index is re-issued, so pinned term ids never change meaning. HDT export reads one pinned snapshot, so a
checkpoint taken under concurrent writes is internally consistent (NF5).
Per-tuple visibility (row-level delete) is the next section, delivered under
`SPEC-25` S1.

`Store::pin()` hands the same pinned tier state out as an **owned**
`PinnedSnapshot`, detached from the `&Dictionary` borrow `snapshot()` carries;
`Store::snapshot_at(&pin)` re-opens it as a full `StoreSnapshot` as often as
needed, always at the pinned version (`PinnedSnapshot::repin`). That is the
seam a caller needs to keep one read version alive across many reads without
holding a lock on the store — `horndb-sparql`'s per-query pinned read view
(HDB-119) is the first user.

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
- `retract_quad_batch` and `compact()` still rebuild a partition row by row —
  they have to touch every row anyway — so they force the merge first.
  `apply_quad_batch` rebuilds only the predicates a batch actually deletes
  from; see the next section.

**A read pays for the merge, but no longer blocks a write** (HDB-122). The
merging thread is a reader, and it still pays the whole cost — a sort of every
row, plus a second sort for the object-major layout above `hot_threshold`, order
of seconds on a 10M-row predicate. Between HDB-84 and HDB-122 it also held the
partition's `runs` mutex for that whole merge, so a concurrent
`with_appended_rows` or `mark_live` on that partition waited it out: a
reader-blocks-writer stall that did not exist before, and one `MemoryTier`'s
writer mutex could not bound because the merging thread never takes that mutex.

The merge now runs outside the mutex: `PredicatePartition::merged_cols` clones
the run list (`Arc` clones) under the lock, releases it, merges, then re-takes
the lock to swap the collapsed one-run list in. Two things make the swap safe.
`OnceLock::get_or_init` runs the merge on exactly one thread per partition, so
two readers cannot merge concurrently. And a writer never mutates the partition
it appends to — `with_appended_rows` clones the run list into a **new**
`PredicatePartition` — so an append in flight during a merge cannot be lost by
the swap; whether the writer's clone catches the pre- or post-merge list, both
hold the same rows.

Each merge is timed into `horndb_storage_partition_merge_seconds` and counted by
`horndb_storage_partition_merges_total{trigger}` (`read` vs `write_cap`), so the
tail is attributable — see `docs/metrics.md`. A reader still blocks *other
readers* of the same partition for the merge's duration (they wait on the
`OnceLock`); that is unchanged and is not a write-path stall.

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

The hot-predicate threshold is `HORNDB_HOT_THRESHOLD` (`partition::hot_threshold`
/ `set_hot_threshold`, default 1,000,000 live rows; `off` disables eager
materialisation). It decides only *when* a partition builds its object-major
layout — at build time, or on the first object-major read — never what the
partition contains, so it is safe to move at any point. Resolved once per
process; `MemoryTier::with_hot_threshold` still overrides it per tier.
Measured: eager costs 0.71s of a 10M-triple load and no crate above
`horndb-storage` reads the object-major layout today (`docs/benchmarks.md`,
"Cutting the `apply_quad_batch` hash tables").

## `apply_quad_batch` append-run path (HDB-102, delivered)

`apply_quad_batch` chooses its write strategy **per predicate**, not per batch:

- **No deletion targets this predicate** → the append-run path above. The pairs
  that are not already live become one extra run
  (`PredicatePartition::with_appended_rows`); nothing already stored is read,
  copied, or re-sorted, and the merge happens on the first read. This covers
  every add-only batch — which is every `Store::insert_quads`, every SPARQL
  `INSERT DATA`, and every `INSERT … WHERE` that deletes nothing — and also the
  add-only predicates of a mixed batch.
- **This predicate has deletion targets** → the pre-existing rebuild: carry
  every row forward into a fresh `PartitionBuilder`, end-stamping the matches.

Why the deletion side keeps the rebuild. A deletion sets `end` on a row that
lives *inside* an existing run, and runs are immutable `Columns` blocks shared
by `Arc` with every snapshot an older reader pinned. Writing the stamp in place
would rewrite history under those readers, so the only in-design options are to
rebuild the partition (what it does) or to give `Columns` a per-row mutable end
column with its own versioning — a redesign of the MVCC row representation, not
a local change. Nothing about the append-run path forecloses that later.

`ApplyReport::inserted` stays exact on both paths: `Store::insert_quads` returns
it and SPARQL `INSERT DATA` idempotency is decided by it. The fast path gets it
from `PredicatePartition::mark_live`, which answers "which of these sorted pairs
are already live?" with one galloping search per run and **without** merging the
partition. Going through the merged view instead would force the whole-partition
merge on every write and give the O(existing)-per-call cost straight back.

`mark_live` is sound because merging never changes a pair's liveness:
`Columns::sort_dedup` leaves end stamps alone and only collapses duplicate live
rows for one pair. So a pair is live in the merged view iff some run holds a live
row for it.

Load-phase metrics on the fast path: `copy_forward` gets nothing (no rows are
carried), the `mark_live` probe is charged to `merge`, and
`with_appended_rows` to `build` — matching `insert_quad_batch`. See
`docs/metrics.md`.

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

## Dictionary GC (HDB-121, delivered)

The dictionary used to be purely append-only: deleting every triple that
mentioned a term freed neither its id nor its lexical bytes, so a continuous
append + retract workload grew dictionary memory for the life of the process
however few triples were live. `Store::compact()` now runs a **mark and sweep**
over the dictionary right after it reclaims dead rows.

- **Mark, not refcount.** `TierSnapshot::for_each_term_id` walks the rows that
  survived compaction — every graph id, every predicate id, and the subject and
  object of every *physically present* row, dead-but-unreclaimed rows included.
  A refcount would have to be maintained on every insert, retract and partition
  rebuild to save a walk that runs beside a compaction which has just touched
  the same rows.
- **Liveness bound: the tier's own `min_pinned`.** `compact()` keeps every row
  with `end > min_pinned`, so marking what survives it marks everything any
  pinned reader can still resolve. No second liveness scheme.
- **What is freed:** the reverse-vector `Term` and the forward-map key — the two
  allocations that dominate footprint. What stays is an empty `Option<Term>`
  slot per reclaimed index.
- **Ids are NOT re-used.** A thread that has interned a term but not yet
  installed its rows holds an id no snapshot version can see, so `min_pinned`
  does not cover id reuse the way it covers row reclamation. Reuse would be
  silent corruption, so the index stays consumed: `Dictionary::len()` (index
  space, monotonic, still capped at `MAX_DICT_INDEX = 1<<60`) and
  `Dictionary::live_len()` (resolvable terms) diverge after the first sweep.
  Upgrade path: register id issuance with the reclaimer (an epoch or refcount
  taken at `intern`, released when the batch commits), then hand the free slots
  back out. The free set is implicit — the `None` slots themselves — so nothing
  extra has to be tracked in memory for that.
- **Precondition, same reason:** the *sweep* assumes no thread holds a `TermId`
  it has not yet installed rows for. Every `Store` write path interns and
  installs inside one call, and the sweep aborts if a write commits while it is
  marking, so the exposure is the id-based entry points and the bulk loaders'
  parse-thread interning. Compaction is an explicit, quiesced maintenance call
  (HDB-63); do not wire it to a timer before closing that gap. Row reclamation
  carries no such precondition.
- **The persistent base (next section) carries the tombstones**: a reclaimed
  index is a zero-length slot in the base file, reloads as reclaimed, and the
  file header carries the freed count for the gauge.

Metrics: `horndb_storage_dictionary_terms` (index space consumed) versus
`horndb_storage_dictionary_terms_live` (resolvable terms) — `docs/metrics.md`.
Test: `tests/dictionary_gc.rs`. Bench: `benches/dict_gc_churn.rs`.

## Persistent dictionary (SPEC-25 S2, delivered)

`Dictionary::flush(path, next_bnode_doc_tag)` — normally reached as
`Store::flush_dictionary(path)` — writes every index the dictionary has issued
to one file; `Dictionary::open(path)` maps it back; `Store::with_dictionary`
puts an empty tier over it. Plan: `docs/plans/PLAN-25-02-persistent-dictionary.md`.

- **Layout** (`dict_base.rs`): 64-byte header, a `u64` offset table, a
  `snapshot::term_codec` arena, an `fst::Map` from those bytes to `TermId`
  bits. id → term is one offset indirection; term → id is the FST. Built under
  a unique temp name beside the target (`<name>.<pid>.<n>.tmp`, removed on
  error), renamed, then the directory is fsynced: `Ok` means the new base is
  durable, a mapping of the previous file stays valid, and a reader never
  sees a partial file. `open` checks the header, section lengths, and the two
  end offsets, with the arithmetic in checked `u64`; a slot whose offsets do
  not fit the arena reads as `None`, never a panic. `Dictionary::verify()` is
  the opt-in full check (every offset, FST checksum) — run it after copying a
  base between hosts.
- **Blank-node document tag travels in the header** (bytes 56..64).
  `Store::flush_dictionary` writes the store's `next_bnode_doc_tag` counter
  and `Store::with_dictionary` seeds from it, so a document loaded into a
  reopened store never shares `_:b1` with a document the base holds. Test:
  `distinct_documents_across_reopen_keep_distinct_blank_nodes`.
- **Base + overlay.** Indices `1..=base_len` resolve through the mapping;
  the in-memory forward/reverse maps hold only what this process interned,
  numbered from `base_len + 1`. A fresh dictionary has `base_len == 0` and
  runs the pre-S2 code on every hit, and a reopened one hands a new term the
  id it would have got without the restart.
- **Probe order is overlay, then base — load-bearing.** A base term `gc`
  reclaimed and later re-interned lives in the overlay under a new id while
  the base FST still maps its bytes to the dead one. Dead base indices are
  kept in a `RoaringTreemap` beside the overlay so both directions answer
  "nothing" for them; the next flush writes them as tombstones.
- **Keyed on `term_codec`, not the forward-map key.** The HDB-95 key
  substitutes first-seen side-table ids and is not order-stable across
  processes; the base uses the self-contained encoding instead. A base probe
  therefore encodes the term twice on an overlay miss (compact key, then
  codec), which is the price of keeping the overlay's hit path unchanged.
- **`get` falls through to the base** even when the term carries a datatype
  IRI or language tag this process has never seen — that no longer proves the
  term is absent.
- **Deferred.** The running process keeps using its overlay after a flush
  (the merged file is for the next `open`), so overlay memory is released at
  restart, not at checkpoint. Ids interned after the flush are not in the
  file, and a process reopened on it re-issues them to whatever comes next —
  the WAL (below) orders dictionary appends against the flush so a replayed
  quad never names an id the base gave to a different term. The HDB-93 repeat
  cache (4,096 entries, 4-way, full-hash) is not in: it sits in front of the
  base probe and is a hornbench-measured latency lever.

Tests: `tests/dictionary_persist.rs` (reopen both directions for every term
kind, `GraphId` stability, tombstones, id continuation, and a differential
reload into a reopened store that must allocate no ids). Bench:
`benches/dict_persist.rs` (`audit-pass.sh` leg `dict_persist`).

## Write-ahead log + crash recovery (SPEC-25 S3, delivered)

`Store::open(dir)` (or `open_with(dir, SyncPolicy)`) gives a store that
survives a kill; `Store::in_memory` and `Store::with_dictionary` have no log
and behave as before. Plan: `docs/plans/PLAN-25-03-wal-crash-recovery.md`;
code: `src/wal.rs` plus the `logged` wrapper in `store.rs`.

- **Layout.** `dir/MANIFEST` holds the generation number (written as
  temp + rename + directory fsync — the checkpoint's commit point);
  `dir/dict.<gen>` is the S2 dictionary base at that checkpoint (absent for
  gen 0); `dir/wal.<gen>` holds the records since. Files of any other
  generation are swept on open.
- **Record.** `[u32 body_len][u32 crc32c][body]`; body = `u8 kind` (1
  `Insert`, 2 `Apply`, 3 `Checkpoint`), `u64 version`, `u64 bnode_doc_tag`,
  `u64 dict_first`, `u32` count of `(u32 len, term_codec bytes)` dictionary
  appends, `u32` count of `(g, s, p, o)` dels, the same for adds.
  Little-endian; CRC-32C table in-crate.
- **Write-ahead.** `insert_quad_batch`, `retract`, and `apply` each append
  one record (version = current + 1, dictionary terms `(logged_len,
  dict.len()]`) and fsync per policy *before* the tier write. `Insert` and
  `Apply` replay through the tier's own bump rule (`insert_at` / `apply_at`
  with the logged version), so stamps match; a net-empty apply is logged
  and replays as the same no-bump. The loader's `flush` goes through
  `Store::insert_quad_batch`, so bulk loads are logged too.
- **Dictionary replay does not `intern`.** `Dictionary::replay_append(index,
  term)` puts the term at the logged index unconditionally, freeing a stale
  slot that still holds it (GC is not logged, but a term freed and
  re-interned before the crash was logged under its new index).
  `Store::compact()` logs pending appends before running the GC so no
  unlogged index is freed. Recovery may keep dead rows and dead dictionary
  slots a pre-crash compaction had reclaimed — "modulo compaction", as the
  spec allows.
- **Checkpoint.** `Store::checkpoint()`: flush the dictionary to
  `dict.<gen+1>`, dump the rows visible at the pinned version as
  `Checkpoint` records (1M-row chunks, at least one record so the commit
  clock is carried) into `wal.<gen+1>`, fsync, switch `MANIFEST`, unlink the
  old generation. Rows the checkpoint carried restart at `begin = checkpoint
  version` and dead rows are dropped — the physical stamps of pre-checkpoint
  rows are not bit-identical after a restart, the visible quads and the clock
  are.
- **Tail handling.** A record cut short by EOF, or a last record with a bad
  checksum, is a torn tail: dropped, file truncated to the last good record.
  A bad checksum with bytes after it is `StorageError::Wal` from `open`, and
  the file is left alone.
- **Fsync policy.** `SyncPolicy::EveryBatch` (default, window = nothing) or
  `SyncPolicy::Every(Duration)` (fsync on the first append after the
  interval; window = records since the last fsync; no timer thread, so a
  quiet store stays unsynced until its next append or `Store::sync_wal()`).
- **Every write is logged, by construction.** `Store::tier()` returns
  `&dyn Tier`, the read half; the write half (`TierWrite`) is only reachable
  through the store's entry points (`insert_quads`, `retract_quads`,
  `apply_quads`, `apply_quad_ids`, the loader). A caller holding ids — the
  SPARQL `CLEAR`/`DROP GRAPH` sweep — uses `Store::apply_quad_ids` with
  quads from `Dictionary::quad_from_ids`. Replay runs the tier's own insert
  and apply paths, so it is charged to the `storage_load_phase_*` metrics
  like a bulk load.
- **Call site for HDB-51 (`serve`).** `HornBackend::with_store(Store::open(
  &data_dir)?)` at startup; call `Store::checkpoint()` on a schedule
  (SPEC-24 S5 owns the cadence) and on clean shutdown. Not done here. Also not in: a directory lock against two processes, WAL metrics.
- **SPEC-24 S5 input records** (HDB-52, ADR-0018). Kinds `Input` (4) and
  `TickCommit` (5) share the framing and the fsync policy but carry their own
  bodies, so `decode` returns `Record::Input` / `Record::TickCommit` instead of
  a `BatchRecord` and replay hands them to the circuit, not the tier. Surface:
  `Store::log_input`, `Store::log_tick_commit` (always syncs — the "per-tick"
  fsync policy), `Store::take_recovered_inputs` / `has_recovered_inputs`. A
  checkpoint's generation roll drops them, which is the log truncation SPEC-24
  S5 pairs with the drain.

Tests: `tests/wal_recovery.rs` (crash after append with ids, quads, version
and stamps compared; id differential across recovery; checkpoint → append →
reopen; torn tail; corrupted middle record; compaction between records;
timed policy; stale generation sweep). Bench: `benches/wal_append.rs`
(`audit-pass.sh` leg `wal`).

## Cold partitions (SPEC-25 S5, first leaf delivered)

SPEC-25 S5 asks for "a read-only `Tier` impl over the snapshot encoding". The
tree does not support that reading. `Tier`'s only data accessor,
`Tier::predicate`, returns `None` on `MemoryTier` and nothing calls it; every
real read goes through `MemoryTier::snapshot()` and the `TierSnapshot`
accessors. **So the seam is the partition, not the tier.** `partition::Partition`
is a `Warm(PredicatePartition) | Cold(ColdPartition)` enum, and a `GraphStore`
maps each predicate to one. `TierSnapshot` stays the single snapshot type every
caller already uses; a second `Tier` impl would give the executor nothing to
call.

The whole-store snapshot encoding (`snapshot/format.rs`) is not reusable
either: it is a `Read` stream with one front-coded dictionary of per-snapshot
dense local ids and no offsets. `cold.rs` defines a per-partition file over
global `TermId` bits that reuses the varint codec and the adjacency shape (see
its module docs for the byte layout). Only the subject-major block is stored;
object-major reads decode and re-sort transiently, the same shape
`PredicatePartition::ordered_at` already uses on its filtered branch. A second
block would roughly double the file.

**Why a cold partition needs no visibility stamps.** `MemoryTier::demote`
encodes exactly the rows visible at the version it runs at, and swaps the new
`TierSnapshot` in at that *same* version — demotion is maintenance, not a
logical write, like `compact()`. Every read goes through a pinned
`TierSnapshot` and passes that snapshot's own version as `at`; there is no API
to read a snapshot at any other version (`Store::snapshot_at` takes a pin, not
a number). So a snapshot that can see the cold partition was created at or
after the swap and its version is `>=` the encoded one. A reader pinned before
the swap keeps its own older `Arc<GraphStore>` and still sees the warm
partition, unchanged.

**Writes never land cold.** `insert_at`, `apply_at` and `retract_quad_batch`
route a cold partition through `warm_for_write`, which promotes it back to a
`PredicatePartition` first — they need the stamp columns, which only warm has.
That is also what keeps the paragraph above true: a retraction after a demotion
promotes, so no row in a cold file is ever end-stamped behind its back.

**Interaction with dictionary GC (HDB-177).** A cold encoding holds only the
rows visible at demotion time, so `demote` refuses (`Ok(false)`) while the
partition still physically holds a dead row a pin below the compaction
horizon needs (`Columns::has_retractions()` after `compact()`'s pass) —
otherwise dictionary GC's mark (see "Dictionary GC" above; it walks
`for_each_term_id` over whatever the *current* snapshot holds) would stop
seeing that row's term ids and free them out from under the pin, resolving as
`InvalidTerm` on the next read. The demotion is only postponed: it succeeds
once the pin drops and a later `compact()` reclaims the row.

**Not durable.** Nothing records which predicates were demoted, so `Store::open`
deletes `<dir>/cold` and replays every partition warm. Durable placement needs a
manifest record; see the `ponytail:` comment in `Store::open_with`.

Placement policy, access statistics, and the `HotSetAdvisor` bias are separate
tasks. `TierStats.bytes_estimated` stays the warm+cold total; `bytes_cold`
carries the same cold-partition sum on its own (SPEC-25 S5, HDB-178), so a
caller that wants warm alone computes `bytes_estimated - bytes_cold`.
