---
status: executed
date: 2026-09-04
scope: "SPEC-25 S2 — persistent on-disk dictionary: immutable mmap base (FST + offset table + term arena) under the existing in-memory overlay, merged into a new base file at checkpoint; reopen without re-interning"
---

# Persistent on-disk dictionary (SPEC-25 S2) Implementation Plan

**Goal:** `Dictionary::flush(path)` writes every term this dictionary has ever
issued to one file; `Dictionary::open(path)` maps it back and keeps issuing ids
from where the file stopped. A reopened store resolves id → term and term → id
for the whole corpus without re-interning it, and a term interned after reopen
gets the id it would have got without the restart.

**Structure (settled by HDB-93, `docs/benchmarks.md` "Which structure backs the
mapped dictionary base"; not re-benched here):**

- term → id: `fst::Map` over the `snapshot::term_codec` bytes of every live
  term, value = the `TermId` bits. The FST spells the datatype IRI out, so it
  is reopen-order independent (SPEC-25 §S2 "build the mapped base on the
  snapshot term encoding, not on the in-memory dictionary key").
- id → term: a flat `u64` offset table over a byte arena of the same encoding.
  One indirection, O(1). Slot `i` (index `i + 1`) is `arena[off[i]..off[i+1]]`;
  a **zero-length slot is a tombstone** — the reload-as-reclaimed bit HDB-121
  asked for, with no extra column.
- One file, four sections: 64-byte header, offsets, arena, FST. Mapped once.

**What does not change:** the in-memory overlay (`DashMap` forward map keyed on
the HDB-95 compact key, `Vec<Option<Term>>` reverse) and everything HDB-106
instrumented. Overlay ids are numbered from `base.slots + 1`, so with no base
the code path and every id are byte-identical to today. NF3 (single-probe
overlay, inline ints never probe) stands.

**Probe order:** overlay forward map, then base FST. Required, not a
preference: a base term GC'd and re-interned lives in the overlay under a new
id while the old FST still maps its bytes to the dead id.

**Not in this plan (ponytail, stated ceilings):**

- `flush` does **not** swap the new base into the running process; the
  overlay keeps its terms until restart. Upgrade path: rebuild under the
  reverse write lock and `forward.retain(payload > flushed)`.
- No repeat cache (HDB-93 item 1). It is a latency lever in front of the
  base probe, measurable only on hornbench, which is offline for this task.
  Filed as a follow-up in `docs/architecture.md`.
- No WAL ordering of dictionary appends — SPEC-25 S3.

## Tasks

- [x] 1. `crates/storage/src/dict_base.rs` — `MappedBase { mmap, slots, freed, fst }`,
      `open(path)`, `term_bytes(index) -> Option<&[u8]>` (None for tombstone /
      out of range), `get(codec_bytes) -> Option<TermId>`, and
      `write(path, slots: impl Iterator<Item = Option<(&[u8], TermId)>>)`
      building offsets + arena + FST through a temp file and `rename`.
      Deviation: the item is `Result<Option<(Vec<u8>, TermId)>>` — owned
      bytes because the keys are sorted for the FST after the arena is
      written, `Result` so a damaged base slot fails the flush instead of
      becoming a tombstone. Fix round (PR #343 review): unique temp name
      `<name>.<pid>.<n>.tmp` + directory fsync, checked header arithmetic
      and offset sentinels at `open`, opt-in `verify()`, inline-int ids
      refused, and the blank-node document tag in header bytes 56..64
      (`Store::flush_dictionary` / `Store::with_dictionary`).
      Move `fst` / `memmap2` from dev-deps to deps in `crates/storage/Cargo.toml`.
- [x] 2. `dictionary.rs` — `base: Option<MappedBase>`; reverse lock guards
      `Overlay { terms, base_dead: RoaringTreemap }`; `Dictionary::open`,
      `Dictionary::flush`, `base_len`; base fallthrough in `get` / `intern` /
      `lookup` / `lookup_batch` / `numeric_value` / `issued`; `gc` marks base
      slots dead instead of taking them; `len`/`live_len` count the base.
      `Store::with_dictionary(Dictionary)`.
- [x] 3. `tests/dictionary_persist.rs` — reopen round trip (both directions,
      every term kind incl. a typed literal whose datatype the reopened
      process has never seen, a named graph's `GraphId`, a GC tombstone that
      reloads as reclaimed, fresh interning continuing at `len + 1`), and a
      differential: load the N-Triples fixture into a fresh store, flush, reopen,
      load it again → identical term ids and no new ids (the
      `parallel_loader.rs::assert_same_store` pattern).
- [x] 4. `benches/dict_persist.rs` (criterion: flush, open, base term → id,
      base id → term over a synthetic LUBM-shaped key set) and a `dict_persist`
      leg in `scripts/bench/audit-pass.sh` so `bench.yml` can run it on
      hornbench.
- [x] 5. Docs, same commit: `docs/architecture.md` S2 row, `docs/benchmarks.md`
      S2 row (pending hornbench), `crates/storage/INTEGRATION-NOTES.md`,
      `lib.rs` module doc, this plan → `executed`.

## Verification

`cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`;
`cargo nextest run -p horndb-storage -p horndb-owlrl -p horndb-incremental`;
`cargo nextest run -p horndb-sparql --features server`; harness selected subset
(`--engine owlrl run`, 525/0).
