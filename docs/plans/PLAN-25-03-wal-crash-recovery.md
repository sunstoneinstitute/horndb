---
status: executed
date: 2026-09-04
scope: "SPEC-25 S3 — write-ahead log + crash recovery: one checksummed append-only log per store directory, generation-numbered checkpoints (dictionary base + row dump) switched atomically through a MANIFEST, replay on open to identical ids and contents"
---

# Write-ahead log + crash recovery (SPEC-25 S3) Implementation Plan

**Goal:** `Store::open(dir)` gives a store that survives a kill. Every
committed batch is one checksummed record in `dir/wal.<gen>`, appended and
fsynced (per the policy) *before* the batch is applied. `Store::checkpoint()`
flushes the dictionary, dumps the visible rows, and switches generation
atomically. Reopen replays the log past the checkpoint and arrives at the same
term ids, the same visible quads, and the same visibility stamps on every row
committed after the checkpoint.

**Layout (settled here; ADR-0018 asked for one physical log):**

- `dir/MANIFEST` — decimal generation number. Written by temp + `rename` +
  directory fsync; it is the checkpoint's commit point.
- `dir/dict.<gen>` — the S2 dictionary base at the checkpoint (absent for
  gen 0, the empty store).
- `dir/wal.<gen>` — records, `[u32 body_len][u32 crc32c][body]`. Body:
  `u8 kind` (1 `Insert`, 2 `Apply`, 3 `Checkpoint`), `u64 version`,
  `u64 bnode_doc_tag`, `u64 dict_first`, `u32 dict_count` × `(u32 len,
  term_codec bytes)`, `u32 n_dels` × `(g, s, p, o)`, `u32 n_adds` × the same.
  Checkpoint records open the file: the rows visible at the checkpoint
  version, in 1M-row chunks, with no dictionary appends (the base has them).
  Little-endian throughout; CRC-32C (Castagnoli) table in `wal.rs`, no new
  dependency.

**Dictionary ordering (the S2 hazard):** each record carries every term
interned since the last record (`(logged_len, dict.len()]`, read under the
reverse lock, before the batch is applied), so no committed row ever names
an id the log has not spelled out. Replay interns them in index order and
refuses a record whose intern lands on a different index. A checkpoint sets
`logged_len` to the flushed slot count, so terms interned after the flush are
in the next record, never re-issued. `Store::compact()` on a WAL-backed store
logs pending appends first, so the dictionary GC cannot free an index the log
has not seen.

**Replay versions:** `MemoryTier` gains crate-private `insert_at` /
`apply_at` taking the commit version explicitly (the public `Tier` methods pass
`None` and behave exactly as before) and `set_version` for the checkpoint. An
`Insert` record replays through the insert path (always bumps), an `Apply`
record through the apply path (bumps iff net non-empty) — the same rule the
live write used, at the same version, so stamps match. Records are written
ahead with version `current + 1`; a net-empty apply leaves the tier where it
was, and so does its replay.

**Tail handling:** a record cut short by the file end, or the last record with
a bad checksum, is a torn tail: dropped, and the file is truncated to the last
good record so the next append does not bury it. A bad checksum with bytes
after it is `StorageError::Wal` — never a panic.

**Fsync policy:** `SyncPolicy::EveryBatch` (default; window = nothing) or
`SyncPolicy::Every(Duration)` (fsync on the first append after the interval;
window = records since the last fsync). `Store::sync_wal()` forces one.
ponytail: no timer thread — a quiet store under `Every(d)` stays unsynced
until its next append or an explicit sync.

**Not in this plan (ponytail):** wiring to the `serve` binary (SPEC-26
layering; the call site is `Store::open` + `Store::checkpoint`, recorded in
`crates/storage/INTEGRATION-NOTES.md` for HDB-51); SPEC-24 S5 `Input` /
`TickCommit` record kinds (the kind byte leaves room); checkpoint scheduling;
a directory lock against two processes; WAL metrics.

## Tasks

- [x] 1. `crates/storage/src/wal.rs` — CRC-32C, `Record` encode/decode,
      `Wal { dir, gen, file, logged_len, policy, last_sync }` with `open`
      (replay iterator + tail truncation), `append`, `checkpoint`
      (dict flush → wal.<gen+1> → MANIFEST → unlink old), `sync`.
      `StorageError::Wal`.
- [x] 2. `memory_tier.rs` — `insert_at` / `apply_at(…, Option<u64>)`,
      `set_version`. `dictionary.rs` — `terms_after(logged)` for the
      appended-term range and `replay_append(index, term)` for replay. `store.rs` — `Store::open` / `open_with(dir,
      SyncPolicy)` / `checkpoint` / `sync_wal`; the `logged` helper around
      the three tier write paths; the loader's `flush` goes through it.
- [x] 3. `tests/wal_recovery.rs` — crash after append (kill = forget the
      store, no checkpoint; reopen: ids, quads per graph, version, stamps
      identical); torn tail; corrupted middle record; checkpoint → append →
      reopen (old generation gone); id differential across recovery (reload
      the fixture into the recovered store: same ids, no new ones); `Every`
      policy round trip.
- [x] 4. `benches/wal_append.rs` (append under both policies; replay) +
      `audit-pass.sh` leg `wal`.
- [x] 5. Docs, same commit: `docs/architecture.md` S3 row, `docs/benchmarks.md`
      (pending hornbench), `docs/index.md`, `crates/storage/INTEGRATION-NOTES.md`,
      SPEC-25 §S3 delivered note, this plan → `executed`.

## Deviations found while executing

- Replay cannot use `Dictionary::intern`: a term the live GC freed and a
  later batch re-interned is logged under its new index, but replay does not
  replay compaction, so `intern` would return the stale index.
  `Dictionary::replay_append` appends at the logged index unconditionally
  and frees the stale slot. Test: `compaction_between_records_keeps_the_log_replayable`.
- A net-empty apply *is* logged (the write-ahead record is written before
  the outcome is known) and replays through the same no-bump path.
- Rows restored from a checkpoint carry `begin = checkpoint version`; the
  test compares quads, version and dictionary length across a checkpoint,
  and full stamps only across a plain crash.

- Review round (PR #345): `CLEAR`/`DROP GRAPH` wrote through
  `Store::tier()` and bypassed the log, so the next batch could never replay.
  `Tier` is now the read half and `TierWrite` the write half; `Store::tier()`
  returns `&dyn Tier`, and the sweep goes through `Store::apply_quad_ids`.
  The directory is fsynced after the first `wal.<gen>` is created;
  `MANIFEST.*.tmp` leftovers are swept on open.

## Verification

`cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`;
`cargo nextest run -p horndb-storage -p horndb-owlrl -p horndb-incremental`;
`cargo nextest run -p horndb-sparql --features server`; harness selected subset
(`--engine owlrl run`, 525/0).
