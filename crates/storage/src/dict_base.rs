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
//!
//! The encoding is `term_codec`, not the in-memory forward-map key: that key
//! substitutes first-seen side-table ids for datatype IRIs and language tags,
//! so its bytes depend on load order and are not a persistence format (see
//! `dictionary.rs`). `term_codec` spells everything out.
//!
//! [`Dictionary`]: crate::Dictionary
//! [`Dictionary::gc`]: crate::Dictionary::gc

use crate::error::{Result, StorageError};
use crate::term::TermId;
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"HDBDICT\0";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 64;

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

pub(crate) struct MappedBase {
    mmap: Arc<Mmap>,
    slots: u64,
    freed: u64,
    offsets: Range<usize>,
    arena: Range<usize>,
    fst: fst::Map<MmapRange>,
}

impl MappedBase {
    /// Map `path` and validate its header and section lengths.
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
        let offsets_len = u64_at(32) as usize;
        let arena_len = u64_at(40) as usize;
        let fst_len = u64_at(48) as usize;
        if offsets_len != (slots as usize + 1) * 8
            || HEADER_LEN + offsets_len + arena_len + fst_len != bytes.len()
        {
            return Err(bad("section lengths do not match the file"));
        }
        let offsets = HEADER_LEN..HEADER_LEN + offsets_len;
        let arena = offsets.end..offsets.end + arena_len;
        let fst_range = arena.end..arena.end + fst_len;
        let fst = fst::Map::new(MmapRange {
            mmap: Arc::clone(&mmap),
            range: fst_range,
        })
        .map_err(|e| bad(e.to_string()))?;
        Ok(Self {
            mmap,
            slots,
            freed,
            offsets,
            arena,
            fst,
        })
    }

    pub(crate) fn slots(&self) -> u64 {
        self.slots
    }

    pub(crate) fn freed(&self) -> u64 {
        self.freed
    }

    fn offset(&self, i: u64) -> usize {
        let at = self.offsets.start + (i as usize) * 8;
        u64::from_le_bytes(self.mmap[at..at + 8].try_into().unwrap()) as usize
    }

    /// The `term_codec` bytes of dictionary index `index` (1-based); `None`
    /// past the end or for a tombstone.
    pub(crate) fn term_bytes(&self, index: u64) -> Option<&[u8]> {
        if index == 0 || index > self.slots {
            return None;
        }
        let (start, end) = (self.offset(index - 1), self.offset(index));
        if start == end {
            return None;
        }
        Some(&self.mmap[self.arena.start + start..self.arena.start + end])
    }

    /// term → id over the `term_codec` bytes of a term.
    pub(crate) fn get(&self, codec_bytes: &[u8]) -> Option<TermId> {
        self.fst.get(codec_bytes).map(TermId)
    }

    /// Write a base file covering `slots` indices, in index order. `None` is
    /// a tombstone. Built under `<path>.tmp` and renamed into place, so a
    /// reader never sees a partial file and an existing mapping of the old
    /// file stays valid.
    ///
    /// ponytail: the live keys are collected in memory to sort them for the
    /// FST — O(dictionary bytes) transient at flush, on top of the terms the
    /// dictionary already holds. Upgrade path: sort slot indices and
    /// re-encode, or merge sorted runs, when a 100M-term flush needs it.
    pub(crate) fn write(
        path: &Path,
        slots: impl Iterator<Item = Option<(Vec<u8>, TermId)>>,
    ) -> Result<BaseStats> {
        let mut offsets: Vec<u64> = vec![0];
        let mut keys: Vec<(Vec<u8>, TermId)> = Vec::new();
        let mut freed = 0u64;
        let mut arena_len = 0u64;
        for slot in slots {
            match slot {
                Some((bytes, id)) => {
                    arena_len += bytes.len() as u64;
                    keys.push((bytes, id));
                }
                None => freed += 1,
            }
            offsets.push(arena_len);
        }
        let n_slots = (offsets.len() - 1) as u64;

        let tmp = path.with_extension("tmp");
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
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
        w.seek(SeekFrom::Start(0))?;
        w.write_all(&header)?;
        w.flush()?;
        w.get_ref().sync_all()?;
        drop(w);
        std::fs::rename(&tmp, path)?;

        Ok(BaseStats {
            slots: n_slots,
            freed,
            arena_bytes: arena_len,
            fst_bytes: fst_len,
            total_bytes: HEADER_LEN as u64 + (n_slots + 1) * 8 + arena_len + fst_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::TermKind;

    #[test]
    fn round_trips_slots_and_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        let a = TermId::new(TermKind::Uri, 1);
        let c = TermId::new(TermKind::PlainLiteral, 3);
        let slots = vec![
            Some((b"\x00http://ex/a".to_vec(), a)),
            None,
            Some((b"\x02hello".to_vec(), c)),
        ];
        let stats = MappedBase::write(&path, slots.into_iter()).unwrap();
        assert_eq!((stats.slots, stats.freed), (3, 1));
        assert_eq!(stats.total_bytes, std::fs::metadata(&path).unwrap().len());

        let base = MappedBase::open(&path).unwrap();
        assert_eq!(base.slots(), 3);
        assert_eq!(base.freed(), 1);
        assert_eq!(base.term_bytes(1), Some(&b"\x00http://ex/a"[..]));
        assert_eq!(base.term_bytes(2), None, "tombstone");
        assert_eq!(base.term_bytes(3), Some(&b"\x02hello"[..]));
        assert_eq!(base.term_bytes(0), None);
        assert_eq!(base.term_bytes(4), None);
        assert_eq!(base.get(b"\x00http://ex/a"), Some(a));
        assert_eq!(base.get(b"\x02hello"), Some(c));
        assert_eq!(base.get(b"\x00http://ex/zzz"), None);
    }

    #[test]
    fn empty_base_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.base");
        MappedBase::write(&path, std::iter::empty()).unwrap();
        let base = MappedBase::open(&path).unwrap();
        assert_eq!(base.slots(), 0);
        assert_eq!(base.get(b"x"), None);
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
        MappedBase::write(
            &path,
            std::iter::once(Some((b"\x00x".to_vec(), TermId::new(TermKind::Uri, 1)))),
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(MappedBase::open(&path).is_err());
    }
}
