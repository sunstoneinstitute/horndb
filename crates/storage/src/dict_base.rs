//! The immutable, memory-mapped base of a persistent [`Dictionary`]
//! (SPEC-25 S2, `docs/plans/PLAN-25-02-persistent-dictionary.md`).
//!
//! One file, four sections, mapped once and never written while mapped:
//!
//! ```text
//! [header 64 B][offsets: (slots + 1) x u64 LE][arena: term bytes][fst::Map]
//! ```
//!
//! * id → term is one indirection: slot `i` (dictionary index `i + 1`) is
//!   `arena[offsets[i]..offsets[i + 1]]`, encoded with
//!   [`crate::snapshot::term_codec`]. A **zero-length slot is a tombstone**:
//!   an index [`Dictionary::gc`] reclaimed before the flush. It reloads as
//!   reclaimed and is never re-issued, exactly like an in-memory `None` slot.
//! * term → id is an [`fst::Map`] from the same encoding to the `TermId`
//!   bits. Tombstoned slots are not in it. Chosen over an MPHF plus
//!   fingerprint array by HDB-93 (`docs/benchmarks.md`): 15.7x smaller at
//!   real-corpus rates and 4.5x faster with a cold page cache — reopen is
//!   what this file exists for.
//! * header bytes 56..64 carry the store's next blank-node document tag, so
//!   a reopened store scopes its next document's `_:b1` away from every
//!   document the base already holds.
//!
//! The encoding is `term_codec`, not the in-memory forward-map key: that key
//! substitutes first-seen side-table ids for datatype IRIs and language tags,
//! so its bytes depend on load order and are not a persistence format (see
//! `dictionary.rs`). `term_codec` spells everything out.
//!
//! `open` validates the header and the two offset sentinels only; slot
//! reads bounds-check every range and answer `None` for a damaged one.
//! [`MappedBase::verify`] is the opt-in full check.
//!
//! [`Dictionary`]: crate::Dictionary
//! [`Dictionary::gc`]: crate::Dictionary::gc

use crate::error::{Result, StorageError};
use crate::term::{TermId, TermKind};
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"HDBDICT\0";
/// Bump on any change to the header layout or to the `term_codec` encoding
/// (its `KIND_*` tags included) — an old file must be refused, not misread.
const VERSION: u32 = 1;
const HEADER_LEN: usize = 64;

/// Distinguishes temp files of concurrent flushes in one process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bad(msg: impl Into<String>) -> StorageError {
    StorageError::Snapshot(format!("dictionary base: {}", msg.into()))
}

/// A byte range of a shared mapping, so `fst::Map` can borrow its section
/// of the one file without a copy.
struct MmapRange {
    mmap: Arc<Mmap>,
    range: Range<usize>,
}

impl AsRef<[u8]> for MmapRange {
    fn as_ref(&self) -> &[u8] {
        &self.mmap[self.range.clone()]
    }
}

/// Byte accounting for one written base file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseStats {
    /// Index space the file covers, tombstones included.
    pub slots: u64,
    /// Tombstoned slots.
    pub freed: u64,
    pub arena_bytes: u64,
    pub fst_bytes: u64,
    pub total_bytes: u64,
}

/// One slot handed to [`MappedBase::write`]: `None` is a tombstone, `Err`
/// aborts the flush (a damaged base slot must never become a tombstone).
pub(crate) type Slot = Result<Option<(Vec<u8>, TermId)>>;

pub(crate) struct MappedBase {
    mmap: Arc<Mmap>,
    slots: u64,
    freed: u64,
    next_bnode_doc_tag: u64,
    offsets: Range<usize>,
    arena: Range<usize>,
    fst: fst::Map<MmapRange>,
}

impl MappedBase {
    /// Map `path` and validate its header, section lengths, and the first
    /// and last offset. Every length is checked in `u64` before it becomes
    /// a range, so a header full of `u64::MAX` is refused, not overflowed.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        // SAFETY: the file is only ever produced by `write`, which builds it
        // under a temporary name and `rename`s it into place; nothing writes
        // to a file at this path in place, so the mapping cannot observe a
        // change. A new flush replaces the directory entry and leaves the
        // old inode (and this mapping) intact.
        let mmap = Arc::new(unsafe { Mmap::map(&file)? });
        let bytes: &[u8] = &mmap;
        if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
            return Err(bad("not a dictionary base file"));
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let version = u32_at(8);
        if version != VERSION {
            return Err(bad(format!("unsupported version {version}")));
        }
        let slots = u64_at(16);
        let freed = u64_at(24);
        let offsets_len = u64_at(32);
        let arena_len = u64_at(40);
        let fst_len = u64_at(48);
        let next_bnode_doc_tag = u64_at(56);
        let want_offsets = slots.checked_add(1).and_then(|n| n.checked_mul(8));
        let total = want_offsets
            .and_then(|o| o.checked_add(HEADER_LEN as u64))
            .and_then(|t| t.checked_add(arena_len))
            .and_then(|t| t.checked_add(fst_len));
        if want_offsets != Some(offsets_len) || total != Some(bytes.len() as u64) || freed > slots {
            return Err(bad("section lengths do not match the file"));
        }
        // `total == bytes.len()` bounds every section by a `usize`.
        let offsets = HEADER_LEN..HEADER_LEN + offsets_len as usize;
        let arena = offsets.end..offsets.end + arena_len as usize;
        let fst_range = arena.end..arena.end + fst_len as usize;
        let fst = fst::Map::new(MmapRange {
            mmap: Arc::clone(&mmap),
            range: fst_range,
        })
        .map_err(|e| bad(e.to_string()))?;
        let base = Self {
            mmap,
            slots,
            freed,
            next_bnode_doc_tag,
            offsets,
            arena,
            fst,
        };
        if base.offset(0) != Some(0) || base.offset(slots) != Some(arena_len) {
            return Err(bad("offset table does not span the arena"));
        }
        Ok(base)
    }

    pub(crate) fn slots(&self) -> u64 {
        self.slots
    }

    pub(crate) fn freed(&self) -> u64 {
        self.freed
    }

    pub(crate) fn next_bnode_doc_tag(&self) -> u64 {
        self.next_bnode_doc_tag
    }

    fn offset(&self, i: u64) -> Option<u64> {
        let at = self
            .offsets
            .start
            .checked_add(usize::try_from(i).ok()?.checked_mul(8)?)?;
        let raw = self.mmap.get(at..at.checked_add(8)?)?;
        Some(u64::from_le_bytes(raw.try_into().unwrap()))
    }

    /// The `term_codec` bytes of dictionary index `index` (1-based):
    /// `Ok(None)` for a tombstone, `Err` past the end or for a slot whose
    /// offsets do not fit the arena.
    pub(crate) fn slot(&self, index: u64) -> Result<Option<&[u8]>> {
        if index == 0 || index > self.slots {
            return Err(bad(format!("index {index} is outside 1..={}", self.slots)));
        }
        let range = self.offset(index - 1).zip(self.offset(index));
        let bytes = range.and_then(|(start, end)| {
            let arena = &self.mmap[self.arena.clone()];
            arena.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
        });
        match bytes {
            Some([]) => Ok(None),
            Some(b) => Ok(Some(b)),
            None => Err(bad(format!("index {index}: offsets outside the arena"))),
        }
    }

    /// [`MappedBase::slot`] for lookups: `None` past the end, for a
    /// tombstone, or for a damaged slot.
    pub(crate) fn term_bytes(&self, index: u64) -> Option<&[u8]> {
        self.slot(index).ok().flatten()
    }

    /// term → id over the `term_codec` bytes of a term.
    pub(crate) fn get(&self, codec_bytes: &[u8]) -> Option<TermId> {
        self.fst.get(codec_bytes).map(TermId)
    }

    /// Full integrity check — opt-in, it reads the whole file. Every offset
    /// must be non-decreasing and inside the arena, and the FST checksum
    /// must match. `open` checks only the header and the two end offsets, so
    /// a file of the right shape with damaged contents opens and answers
    /// `None` for the damaged slots; run this after copying a base between
    /// hosts or when a lookup looks wrong.
    pub(crate) fn verify(&self) -> Result<()> {
        let arena_len = self.arena.len() as u64;
        let mut prev = 0;
        for i in 0..=self.slots {
            let off = self
                .offset(i)
                .ok_or_else(|| bad("offset table truncated"))?;
            if off < prev || off > arena_len {
                return Err(bad(format!("offset {i} out of order or past the arena")));
            }
            prev = off;
        }
        self.fst
            .as_fst()
            .verify()
            .map_err(|e| bad(format!("fst: {e}")))
    }

    /// Write a base file covering `slots` indices, in index order. Built
    /// under a unique temp name beside `path` (`<name>.<pid>.<n>.tmp`),
    /// renamed into place, then the directory is fsynced — so `Ok` means
    /// the new base is durable, a reader never sees a partial file, and an
    /// existing mapping of the old file stays valid. The temp file is
    /// removed on every error path.
    ///
    /// ponytail: the live keys are collected in memory to sort them for the
    /// FST — O(dictionary bytes) transient at flush, on top of the terms the
    /// dictionary already holds. Upgrade path: sort slot indices and
    /// re-encode, or merge sorted runs, when a 100M-term flush needs it.
    pub(crate) fn write(
        path: &Path,
        slots: impl Iterator<Item = Slot>,
        next_bnode_doc_tag: u64,
    ) -> Result<BaseStats> {
        let mut tmp_name = path
            .file_name()
            .ok_or_else(|| bad("path has no file name"))?
            .to_os_string();
        tmp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = path.with_file_name(tmp_name);
        let result = Self::write_via(&tmp, path, slots, next_bnode_doc_tag);
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn write_via(
        tmp: &Path,
        path: &Path,
        slots: impl Iterator<Item = Slot>,
        next_bnode_doc_tag: u64,
    ) -> Result<BaseStats> {
        let mut offsets: Vec<u64> = vec![0];
        let mut keys: Vec<(Vec<u8>, TermId)> = Vec::new();
        let mut freed = 0u64;
        let mut arena_len = 0u64;
        for slot in slots {
            match slot? {
                Some((bytes, id)) => {
                    if id.kind() == TermKind::InlineInt {
                        return Err(bad(format!(
                            "index {}: inline integers are never interned (NF3)",
                            offsets.len()
                        )));
                    }
                    arena_len += bytes.len() as u64;
                    keys.push((bytes, id));
                }
                None => freed += 1,
            }
            offsets.push(arena_len);
        }
        let n_slots = (offsets.len() - 1) as u64;

        let mut w = BufWriter::with_capacity(1 << 20, File::create(tmp)?);
        w.write_all(&[0u8; HEADER_LEN])?;
        for off in &offsets {
            w.write_all(&off.to_le_bytes())?;
        }
        for (bytes, _) in &keys {
            w.write_all(bytes)?;
        }
        keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut builder = fst::MapBuilder::new(w).map_err(|e| bad(e.to_string()))?;
        for (bytes, id) in &keys {
            builder
                .insert(bytes, id.0)
                .map_err(|e| bad(e.to_string()))?;
        }
        // `bytes_written()` excludes what `into_inner` appends to finish the
        // automaton, so measure the section from the writer's position.
        let mut w = builder.into_inner().map_err(|e| bad(e.to_string()))?;
        let fst_start = HEADER_LEN as u64 + (n_slots + 1) * 8 + arena_len;
        let fst_len = w.stream_position()? - fst_start;

        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&n_slots.to_le_bytes());
        header[24..32].copy_from_slice(&freed.to_le_bytes());
        header[32..40].copy_from_slice(&((n_slots + 1) * 8).to_le_bytes());
        header[40..48].copy_from_slice(&arena_len.to_le_bytes());
        header[48..56].copy_from_slice(&fst_len.to_le_bytes());
        header[56..64].copy_from_slice(&next_bnode_doc_tag.to_le_bytes());
        w.seek(SeekFrom::Start(0))?;
        w.write_all(&header)?;
        w.flush()?;
        w.get_ref().sync_all()?;
        drop(w);
        std::fs::rename(tmp, path)?;
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        File::open(dir)?.sync_all()?;

        Ok(BaseStats {
            slots: n_slots,
            freed,
            arena_bytes: arena_len,
            fst_bytes: fst_len,
            total_bytes: fst_start + fst_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(bytes: &[u8], id: TermId) -> Slot {
        Ok(Some((bytes.to_vec(), id)))
    }

    fn write_one(path: &Path) {
        MappedBase::write(
            path,
            std::iter::once(live(b"\x00x", TermId::new(TermKind::Uri, 1))),
            0,
        )
        .unwrap();
    }

    fn patch(path: &Path, at: usize, val: u64) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[at..at + 8].copy_from_slice(&val.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn round_trips_slots_and_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        let a = TermId::new(TermKind::Uri, 1);
        let c = TermId::new(TermKind::PlainLiteral, 3);
        let slots = vec![live(b"\x00http://ex/a", a), Ok(None), live(b"\x02hello", c)];
        let stats = MappedBase::write(&path, slots.into_iter(), 7).unwrap();
        assert_eq!((stats.slots, stats.freed), (3, 1));
        assert_eq!(stats.total_bytes, std::fs::metadata(&path).unwrap().len());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temp file left behind"
        );

        let base = MappedBase::open(&path).unwrap();
        assert_eq!(base.slots(), 3);
        assert_eq!(base.freed(), 1);
        assert_eq!(base.next_bnode_doc_tag(), 7);
        assert_eq!(base.term_bytes(1), Some(&b"\x00http://ex/a"[..]));
        assert_eq!(base.term_bytes(2), None, "tombstone");
        assert!(matches!(base.slot(2), Ok(None)));
        assert_eq!(base.term_bytes(3), Some(&b"\x02hello"[..]));
        assert_eq!(base.term_bytes(0), None);
        assert_eq!(base.term_bytes(4), None);
        assert!(base.slot(4).is_err());
        assert_eq!(base.get(b"\x00http://ex/a"), Some(a));
        assert_eq!(base.get(b"\x02hello"), Some(c));
        assert_eq!(base.get(b"\x00http://ex/zzz"), None);
        base.verify().unwrap();
    }

    #[test]
    fn empty_base_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        MappedBase::write(&path, std::iter::empty(), 0).unwrap();
        let base = MappedBase::open(&path).unwrap();
        assert_eq!(base.slots(), 0);
        assert_eq!(base.get(b"x"), None);
        base.verify().unwrap();
    }

    #[test]
    fn rejects_foreign_and_truncated_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        std::fs::write(
            &path,
            b"not a dictionary at all, but long enough to pass the length check 1234567890",
        )
        .unwrap();
        assert!(MappedBase::open(&path).is_err());
        write_one(&path);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(MappedBase::open(&path).is_err());
    }

    #[test]
    fn rejects_overflowing_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        write_one(&path);
        patch(&path, 16, u64::MAX); // slots
        assert!(MappedBase::open(&path).is_err());
        write_one(&path);
        patch(&path, 40, u64::MAX); // arena_len
        assert!(MappedBase::open(&path).is_err());
    }

    #[test]
    fn rejects_offset_sentinels_off_the_arena() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        write_one(&path);
        patch(&path, HEADER_LEN, 1); // offsets[0]
        assert!(MappedBase::open(&path).is_err());
        write_one(&path);
        patch(&path, HEADER_LEN + 8, 1); // offsets[slots] != arena_len
        assert!(MappedBase::open(&path).is_err());
    }

    #[test]
    fn damaged_interior_offset_is_none_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        let slots = vec![
            live(b"\x00a", TermId::new(TermKind::Uri, 1)),
            live(b"\x00b", TermId::new(TermKind::Uri, 2)),
        ];
        MappedBase::write(&path, slots.into_iter(), 0).unwrap();
        patch(&path, HEADER_LEN + 8, u64::MAX / 2); // offsets[1]
        let base = MappedBase::open(&path).unwrap();
        assert_eq!(base.term_bytes(1), None);
        assert!(base.slot(1).is_err());
        assert_eq!(base.term_bytes(2), None);
        assert!(base.verify().is_err());
    }

    #[test]
    fn rejects_corrupt_fst() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        write_one(&path);
        let mut bytes = std::fs::read(&path).unwrap();
        let fst_start = HEADER_LEN + 16 + 2;
        bytes[fst_start + 8] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        // The FST header still parses; only the checksum notices.
        if let Ok(base) = MappedBase::open(&path) {
            assert!(base.verify().is_err());
        }
    }

    #[test]
    fn rejects_inline_int_ids_and_slot_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        let inline = std::iter::once(live(b"\x05\x02", TermId::new(TermKind::InlineInt, 1)));
        assert!(MappedBase::write(&path, inline, 0).is_err());
        let failing = std::iter::once(Err(bad("damaged")));
        assert!(MappedBase::write(&path, failing, 0).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
