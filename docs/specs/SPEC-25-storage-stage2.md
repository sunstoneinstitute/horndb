---
status: approved
date: 2026-07-19
scope: "SPEC-02 Stage 2 — per-tuple MVCC visibility + delete path, persistent on-disk dictionary, WAL + crash recovery, named-graph snapshots, HDT cold tier + tiering seam, deferred Stage-1 acceptance benches; refines SPEC-02, delivers the storage seams SPEC-24 (E2) consumes"
---

# SPEC-02 Stage 2 — storage completeness

**One-line thesis:** Stage 1 proved the columnar, dictionary-encoded layout;
Stage 2 makes the store *honest about time and durability* — a tuple knows when
it exists (per-tuple MVCC), the store survives a crash (WAL), the dictionary
survives a restart (persistent dictionary), snapshots cover the whole store
(named graphs), and data that has cooled down stops paying warm-tier prices
(cold tier + tiering seam).

**Refines:** SPEC-02 (the standing storage contract — its F1–F9, NF1–NF5, and
acceptance criteria stay in force; this spec adds the Stage-2 requirements
S1–S6 below and upgrades NF5 as stated in S3). Delivers the storage-side
seams SPEC-24 consumes: per-tuple visibility + delete path for SPEC-24 S6
([#215](https://github.com/sunstoneinstitute/horndb/issues/215)) and the
on-disk WAL format for SPEC-24 S5
([#214](https://github.com/sunstoneinstitute/horndb/issues/214)). Coordinates
with SPEC-08 (the `HotSetAdvisor` tier-placement input and the `MlProvenance`
column contract in `crates/storage/INTEGRATION-NOTES.md`) and with SPEC-17
(the real per-tier byte accounting deferred from
[#148](https://github.com/sunstoneinstitute/horndb/issues/148)).
**Epic:** [#187](https://github.com/sunstoneinstitute/horndb/issues/187).
Successor to the Stage-1 epic
[#3](https://github.com/sunstoneinstitute/horndb/issues/3).

## Problem — what Stage 1 ships, and where it stops

`horndb-storage` (~3 KLOC; `crates/storage/src/`) implements the SPEC-02
Stage-1 slice: a dictionary with lock-free reads and stable 64-bit ids with
the term kind in the high bits (`dictionary.rs`, `term.rs`; small literals
inlined, no probe needed), predicate-partitioned columnar `(s_id, o_id)`
partitions with all six orderings on demand (`partition.rs`, `ordering.rs`),
quads via `GraphId` with a reserved default-graph sentinel, Roaring-bitmap
subject/object sets, streaming N-Triples/Turtle/N-Quads loaders, and an
HDT-derived default-graph snapshot export/import (`snapshot/`, measured
5.440 B/triple on synthetic LUBM-shaped data). Copy-on-write snapshot
isolation is delivered: `MemoryTier` holds an immutable versioned
`Arc<TierSnapshot>`; a write clones the top-level graph map, rebuilds only the
affected graphs, bumps `version: u64`, and atomically swaps the pointer, so
`Store::snapshot()` pins a stable read view that concurrent writers never
disturb.

Where it stops:

- **The store cannot delete.** The `Tier` trait has `insert_quad_batch` and
  no removal of any kind; nothing in the crate retracts a tuple. SPARQL
  `DELETE DATA` works today only through a tombstone overlay inside
  `horndb-sparql`, invisible to every other storage consumer. SPEC-24 S6
  is blocked on exactly this: `Circuit::snapshot()` cannot be backed onto
  storage while storage has "no delete path at all" (SPEC-24's words).
- **Snapshot isolation is whole-tier, not per-tuple.** Visibility is "which
  `TierSnapshot` pointer did you pin", one version for the entire tier. There
  is no way to evaluate "is this tuple visible at time t" per tuple, so
  mid-tick point reads and storage-backed MVCC snapshots (SPEC-24 S6) are
  inexpressible. This is the SPEC-02 open question ("true MVCC with per-tuple
  visibility is Stage 2") coming due.
- **Nothing survives a process exit.** The dictionary is an in-memory map,
  rebuilt only by re-importing data. There is no WAL: SPEC-02 NF5 explicitly
  allows losing everything since the last checkpoint, and the only checkpoint
  mechanism is an explicit snapshot export. Restart cost is a full re-import.
- **Snapshots cover the default graph only.** `export_snapshot` *errors* on a
  store holding named-graph data (deliberately — no silent data loss), so any
  quad-bearing store has no checkpoint path at all.
- **Tiering is scaffolding.** `Tier` exists precisely so cold tiers could slot
  in ("Stage 2/3 cold tiers (HDT, CXL, NVMe) can slot in behind the same
  interface" — `tier.rs`), but Stage 1 ships exactly one impl, `MemoryTier`.
  There is no placement policy, no promotion/demotion, and the
  `storage_tier_bytes_estimated{tier}` metric emits only `tier="unknown"` —
  the real per-tier byte accounting was explicitly deferred from #148 to this
  epic.
- **Three Stage-1 acceptance rows were never run.** SPEC-02 acceptance 2
  (LUBM-8000 import ≤30 min), 3 (LUBM-8000 fully-warm footprint ≤55 GB), and
  4 at LUBM-8000 scale (`rdf:type` scan ≥80% of STREAM Triad) are marked
  DEFERRED in `crates/storage/STAGE1-ACCEPTANCE.md`. (A hornbench
  `partition_scan` measurement of 2026-07-07 already hit ~104% of Triad —
  SPEC-12 work — but not on the LUBM-8000 corpus the criterion names.)

## Non-goals

- **Changing the Stage-1 contract.** SPEC-02 F1–F9, NF1–NF4, and its
  acceptance criteria stand unchanged; NF5 is *strengthened* (S3), never
  relaxed. Existing consumers (`wcoj`, `owlrl`, `closure`, `sparql`) keep
  working through the same read surface throughout.
- **Hardware placement.** CXL/NVMe/GPU tier placement, `io_uring`/GPUDirect
  I/O paths, and NUMA multi-socket placement are SPEC-09 Stage-3 territory.
  This spec delivers the cold-tier *seam* (a second `Tier` impl and the
  placement policy around it) on plain files/`mmap`; SPEC-09 later slots
  device-specific tiers behind the same seam.
- **Multi-writer transactions.** The concurrency model stays
  concurrent-read / single-writer (SPEC-02 scope). Per-tuple MVCC (S1) is
  about *read visibility*, not about admitting concurrent writers.
- **rdfhdt wire-format compatibility.** The snapshot and cold-tier encodings
  stay HDT-*derived*, our own format (the Stage-1 decision in
  `crates/storage/INTEGRATION-NOTES.md` stands). Cross-tool `.hdt` interop —
  and an HDT bulk-*import* path for foreign files (SPEC-02 F8's HDT input) —
  remain follow-ups to pick up when a consumer needs them.
- **Owning the circuit-side log semantics.** SPEC-24 S5 owns the DeltaLog
  *contract* (ordering, tick-batch atomicity, replay-to-identical-state) and
  its crash tests. This spec owns the on-disk *format and durability
  machinery* the contract runs on. The layering question (one shared log or
  two) is an open question below, to be settled jointly before either side
  builds.
- **Rule/reasoning semantics.** Which derived rows exist is SPEC-04/05/06
  territory; storage persists and versions whatever it is handed.

## Stage-2 requirements

### S1. Per-tuple MVCC visibility + delete path

Give every tuple a lifetime, and give the store a way to end one.

- **Visibility stamps.** Each stored tuple carries begin/end visibility in
  terms of the tier's monotonically increasing commit version (today's
  `TierSnapshot.version: u64` becomes the commit clock): a tuple is visible
  at version `v` iff `begin ≤ v` and (`end` unset or `v < end`). Inserts
  stamp `begin`; retractions stamp `end` (a delete is a stamp, not an
  eviction — physical reclamation is compaction's job, below).
- **Delete path.** `Tier`/`Store` grow a batch retraction
  (`retract_quad_batch` alongside `insert_quad_batch`) with the same batch
  atomicity: one batch = one commit version. Retracting an absent tuple is a
  no-op with an observable count, not an error (idempotent replay matters for
  S3).
- **Read surface stays version-consistent.** All existing read paths —
  partition scans in every ordering, Roaring subject/object sets,
  `triple_count`/`stats`, snapshot export — evaluate visibility at the
  pinned version. Lazily materialized orderings and bitmaps must not leak
  tuples dead at the pinned version or hide tuples live at it.
- **The SPEC-24 S6 contract.** A storage-backed snapshot must support what
  `Circuit::snapshot()` promises its readers today: `contains`, key-ordered
  iteration, `len`/`is_empty`, and an inclusive as-of token
  (`logical_time()`), pinned immutably across concurrent writes and cheap to
  clone. The binding between the circuit's `LogicalTime` and the storage
  commit version (one shared clock vs. a persisted mapping) is designed
  jointly with E2 — SPEC-24 asks for exactly this agreement before either
  side builds.
- **Compaction.** A background/explicit compaction pass reclaims tuples whose
  `end` precedes every pinned snapshot. Compaction never changes any pinned
  view's contents; write amplification stays inside the NF4 budget.
- **Consequence for `horndb-sparql`.** Native retraction supersedes the
  `DELETE DATA` tombstone overlay; the overlay is retired (or reduced to a
  compatibility shim) once this lands.

*Design freedom the plan must settle:* stamp columns vs. delete-bitmap
sidecars per partition; whether today's clone-the-graph-map copy-on-write
write path survives per-tuple stamping or yields to in-place append with
epoch-based reclamation. Both must preserve the pinned-snapshot guarantee;
the choice is benched, not assumed.

### S2. Persistent on-disk dictionary

The dictionary stops being rebuilt-by-reimport and becomes a durable,
memory-mapped structure — the SPEC-02 open question ("a 10B-triple store can
produce a multi-GB dictionary") settled.

- **Durable id assignments.** Term → id assignments survive restart; ids stay
  append-only and never re-bind (the property pinned snapshots and the WAL
  both rely on). A reopened store resolves both directions — id → term and
  term → id — without re-interning the corpus.
- **Candidate structures** (settled by the implementation plan with bench
  evidence, per the SPEC-02 open question): FST / Marisa-trie for the
  term → id direction plus an offset table for id → term; or sorted-string-
  table segments with front-coding, mirroring the snapshot dictionary
  section. Either way: an immutable memory-mapped base plus a small mutable
  in-memory overlay for terms interned since the last flush, merged on
  checkpoint.
- **Budgets restated honestly.** NF3's "O(1), single cacheline" holds for the
  in-memory overlay and inline terms (which never probe). For the mapped base
  the budget is: id → term in O(1) probes (one offset indirection, page-cache
  resident in steady state); term → id in O(term length). The implementation
  plan states measured numbers next to these bounds.
- **Fits the snapshot story.** The snapshot format stores terms by label
  precisely because Stage-1 ids were ephemeral. With durable ids this
  robustness property stays (snapshots remain label-level and portable across
  stores), but a *local* fast-path reopen (mmap the dictionary, skip
  re-interning) becomes the normal restart.

### S3. Write-ahead log + crash recovery

Durability between checkpoints — the SPEC-02 "Stage 2 may add a
per-predicate-partition WAL" open question, resolved affirmatively.

- **What is logged.** Every committed batch (insert or retract, S1) appends
  one sequenced WAL record before the commit version becomes visible.
  Dictionary appends made by the batch are part of the record (or a
  preceding record in the same atomic append), so replay never sees an id
  without its term — the dictionary is at least as durable as the log that
  references it.
- **Format ownership.** The on-disk record format and file layout (single log
  vs. per-predicate-partition segments — the plan decides with bench
  evidence) belong here; SPEC-24 S5 layers the DeltaLog contract on top and
  contributes its own crash tests. Records are checksummed; a torn tail
  truncates cleanly.
- **Fsync policy.** Configurable: per-record / per-batch / timed-group
  commit. The default is per-batch. The policy and its data-loss window are
  documented at the API.
- **Replay.** Recovery = load the last checkpoint (snapshot, S4), then replay
  WAL records since its recorded commit version, arriving at a state
  bit-identical to the pre-crash committed state modulo wall-clock
  timestamps. Replay is idempotent (records carry commit versions; applying
  an already-applied record is a no-op).
- **Checkpoint + truncation.** A successful checkpoint records its commit
  version and truncates the log up to it. Checkpoint *scheduling* cadence is
  SPEC-24 S5's requirement; the truncation and version-recording machinery
  is this spec's.
- **NF5 upgraded.** SPEC-02 NF5's "lost updates between checkpoints are
  acceptable" clause is retired for stores opened with a WAL: after recovery,
  every batch whose WAL record was durably appended (per the configured fsync
  policy) is present. In-memory-only stores (tests, ephemeral use) keep the
  Stage-1 behavior.

### S4. Named-graph snapshot export/import

Close the "export errors on named-graph data" hole so quad-bearing stores
have a checkpoint path.

- **Quad coverage.** Extend the snapshot format with a graphs section:
  export/import cover all named graphs plus the default graph. The
  `has_named_graph_data` error guard disappears; round-trip yields exact
  *quad*-set equality (the Stage-1 triple-set-equality property, extended).
- **Format versioning.** The Stage-1 header already carries a format version;
  quad snapshots bump it. Stage-1 default-graph snapshots remain readable
  (one-way compatibility: new code reads old snapshots; old code cleanly
  rejects new ones — the existing `unsupported snapshot version` path).
- **Compression stance unchanged.** Same HDT-derived, front-coded + gap-coded
  encoding, applied per graph; the ≤6 B/triple cold budget (NF1) now applies
  across the whole quad store. Not rdfhdt wire-compatible (non-goal above).
- **Checkpoint integration.** This is the checkpoint format S3 recovery
  starts from, so quad coverage is a prerequisite for WAL-backed durability
  of any named-graph store.

### S5. HDT cold tier + tiering seam

Fill the `Tier` scaffolding with a second implementation and the policy that
moves data between the two — the seam SPEC-09 later plugs hardware into.

- **Cold tier.** A read-only `Tier` impl over the HDT-derived compact format
  (the snapshot encoding, S4), memory-mapped from disk, serving the same
  scan/lookup surface as `MemoryTier` at ≤6 B/triple (NF1 cold budget) and
  within NF4's ≤2× read amplification over a contiguous encoded scan. Writes
  never land cold: a write touching a cold-resident partition promotes it (or
  overlays it warm) first — the plan picks the mechanism.
- **Placement policy.** Demotion/promotion at predicate-partition granularity
  (per graph), driven by recent-access statistics, with the SPEC-08
  `HotSetAdvisor::predict_hot` bias applied *alongside* the stats, never
  instead of them (`crates/storage/INTEGRATION-NOTES.md` F4 — with
  `ml.enabled = false` behavior is bit-identical to stats-only). Placement
  is observable but not controllable from query plans (SPEC-02 F6 stands).
- **Tier transparency.** SPEC-02 F6's contract is preserved: a triple-access
  call returns the triple regardless of tier; the executor never reaches
  across tiers. Demotion/promotion never changes query results — only cost.
- **Real tier byte accounting.** `storage_tier_bytes_estimated{tier}` starts
  emitting real values per resident tier (warm DRAM, cold mapped bytes) —
  the #148 deferral lands here, with the `docs/metrics.md` rows updated in
  the same commit per the root sync rule. HBM/CXL label values stay reserved
  for SPEC-09.
- **MVCC interplay.** Cold partitions hold only tuples whose visibility is
  settled (live, with no pinned snapshot preceding their `begin`); demotion
  is a compaction product (S1). This keeps visibility evaluation off the
  cold-scan hot path.

### S6. Deferred Stage-1 acceptance benches

Run the three deferred SPEC-02 acceptance measurements on the hornbench
NUMA-pinned host and record them — closing the Stage-1 ledger this epic
inherited.

- **Acceptance 2:** LUBM-8000 (1.1 B triples) N-Triples import wall-clock,
  target ≤30 min.
- **Acceptance 3:** LUBM-8000 fully-warm footprint via
  `Store::report_footprint()`, target ≤55 GB (50 B/triple budget).
- **Acceptance 4:** `rdf:type` partition scan on the LUBM-8000 store vs.
  measured STREAM Triad for the host, target ≥80% — re-run at the corpus
  scale the criterion names (the 2026-07-07 SPEC-12 `partition_scan` number
  of ~104% Triad is evidence the kernel is capable, not a substitute for the
  LUBM-8000 measurement).
- **Recording.** Fill in `crates/storage/STAGE1-ACCEPTANCE.md` rows 2/3/4
  (host, kernel, commit, numbers) and the matching `docs/benchmarks.md`
  rows in the same change. Benches run on hornbench per the root
  instructions — never on a laptop.
- **Honesty clause.** hornbench (Ryzen 7 7700, desktop DRAM) is not the
  spec's reference workstation (EPYC 9354, 12-channel DDR5). Record the
  numbers against the actual host and say so; if a target is missed for
  memory-capacity rather than design reasons, record the largest LUBM scale
  that fits alongside the LUBM-8000 attempt rather than silently
  substituting a smaller corpus.

## Phasing

Each phase is an independently shippable increment, tracked as a sub-issue of
epic [#187](https://github.com/sunstoneinstitute/horndb/issues/187) and
harness-gated (the SPEC-01 selected subset stays green throughout; storage's
existing test suites — snapshot round-trip, six-orderings, concurrent
snapshot tests — extend rather than regress). Implementation plans
(`PLAN-25-MM-*.md`) are written when each increment is picked up.

1. **S1 — per-tuple MVCC visibility + delete path**
   ([#225](https://github.com/sunstoneinstitute/horndb/issues/225)).
   Highest value: unblocks SPEC-24 S6 (#215) and retires the sparql
   tombstone overlay. The version-clock binding is agreed with E2 here.
2. **S2 — persistent on-disk dictionary**
   ([#226](https://github.com/sunstoneinstitute/horndb/issues/226)).
   Independent of S1; prerequisite for cheap restart and for S3's
   id-durability ordering.
3. **S3 — WAL + crash recovery**
   ([#227](https://github.com/sunstoneinstitute/horndb/issues/227)).
   Consumes S1 (retraction records) and S2 (durable ids); coordinates with
   SPEC-24 S5 (#214), which layers the DeltaLog contract on this format.
4. **S4 — named-graph snapshot export/import**
   ([#228](https://github.com/sunstoneinstitute/horndb/issues/228)).
   Small and self-contained; prerequisite for S3 checkpoints of quad-bearing
   stores, but landable in any order relative to S1–S3.
5. **S5 — HDT cold tier + tiering seam**
   ([#229](https://github.com/sunstoneinstitute/horndb/issues/229)).
   Consumes the S4 encoding and S1 compaction; delivers the #148 tier-bytes
   metric deferral.
6. **S6 — deferred Stage-1 acceptance benches**
   ([#230](https://github.com/sunstoneinstitute/horndb/issues/230)).
   Rows 2/3/4 can run immediately on today's store; re-run any row whose
   subsystem S1–S5 materially changes before closing it out.

S2, S4, and S6 can proceed immediately and in parallel. S1 leads the
correctness-critical track; S3 follows S1+S2; S5 comes last among the feature
phases.

## Acceptance criteria

1. **Deletes exist and snapshots are honest (S1).** `retract_quad_batch`
   retracts through every read path (all six orderings, Roaring sets,
   counts, snapshot export); a snapshot pinned before a retraction still
   sees the tuple, one pinned after does not — verified under concurrent
   reader/writer tests. A storage-backed view satisfies the SPEC-24 S6
   surface (`contains`, ordered `iter`, `len`, as-of token) against
   interleaved writes. The sparql `DELETE DATA` overlay is retired or
   reduced to a shim.
2. **Restart without re-import (S2).** A store closed and reopened from disk
   resolves id → term and term → id for the full LUBM-100 dictionary without
   re-interning; reopen time is I/O-bound (mmap + header validation), not
   proportional to corpus re-parse. Measured probe costs are recorded next
   to the S2 budgets.
3. **Kill-and-replay is bit-identical (S3).** A crash test — commit batches,
   kill the process without checkpoint, recover — reproduces the exact
   committed state (triples, quads, dictionary, visibility stamps)
   bit-identical modulo wall-clock timestamps, under each fsync policy's
   documented window. Torn-tail truncation is exercised. SPEC-24 S5's
   contract tests pass against this log.
4. **Quad stores checkpoint (S4).** A store holding named-graph data
   exports and re-imports to exact quad-set equality; a Stage-1
   default-graph snapshot still imports; a version-bumped snapshot is
   cleanly rejected by Stage-1 code paths.
5. **Cold data costs cold prices (S5).** On a LUBM-scale corpus with a
   demotion policy active: demote → query → promote round-trips preserve
   query results exactly (harness selected subset green against a mixed
   warm/cold store); cold-resident bytes/triple ≤6; cold scan read
   amplification ≤2× (NF4); `storage_tier_bytes_estimated` reports real
   per-tier bytes with `docs/metrics.md` in sync.
6. **The Stage-1 ledger closes (S6).** `STAGE1-ACCEPTANCE.md` rows 2/3/4
   carry measured hornbench numbers (host/kernel/commit recorded) and
   `docs/benchmarks.md` matches; each row is PASS, or its miss is explained
   against the honesty clause with the follow-up filed.

## Risks and open questions

- **Per-tuple stamps vs. the copy-on-write write path.** Today's
  clone-the-graph-map CoW gives whole-tier snapshots almost for free;
  per-tuple stamping may make in-place append with epoch reclamation the
  better substrate, which is a bigger rewrite than "add two columns". S1's
  plan must bench both against the NF4 write-amplification budget before
  committing.
- **One log or two.** Storage WAL (base facts) and the circuit's DeltaLog
  (derived deltas) could be one shared sequenced log or two coordinated
  ones. Replay semantics differ (storage replays stamps; the circuit replays
  ticks). To be settled jointly with E2 before S3 or SPEC-24 S5 builds —
  the same open question SPEC-24 records from its side.
- **Version clock unification.** One shared monotonic clock between
  `LogicalTime` and the storage commit version is simpler but couples commit
  paths; a persisted mapping decouples them but must itself be crash-safe
  (it lands in the WAL). Decided in S1 with E2.
- **Dictionary structure choice is workload-sensitive.** FST wins on prefix-
  heavy IRI corpora; SSTable-with-front-coding is simpler and mirrors the
  snapshot encoding. The multi-GB-at-10B-triples sizing means the loser may
  differ by memory, not speed. Bench on real corpora before committing.
- **hornbench capacity.** LUBM-8000 at ≤50 B/triple needs ~55 GB resident;
  if hornbench's DRAM cannot hold it fully warm, acceptance 3 cannot run as
  written on that host. The honesty clause covers reporting, but a capacity
  miss may force either a bigger host or an explicitly rescaled criterion —
  surfaced early in S6, not discovered at the end.
- **Cold-scan decompression vs. NF4.** The ≤2× read-amplification bound over
  a contiguous encoded scan assumes cheap front-coding/VByte decode;
  visibility filtering or ordering materialization on cold data could blow
  it. The S5 mitigation (only visibility-settled tuples go cold) is design
  intent — verify it holds under snapshot-heavy workloads.
- **Schema evolution headroom.** The SPEC-08 `MlProvenance` per-tuple column
  (`crates/storage/INTEGRATION-NOTES.md` F5) is not yet stored. S1's stamp
  layout and S3's record format must leave room for optional per-tuple
  columns so provenance does not force a second format break.
