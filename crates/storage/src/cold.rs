//! Cold predicate partitions: one predicate's settled rows, encoded once and
//! read back through a memory map (SPEC-25 S5).
//!
//! A cold partition is the second thing a graph can map a predicate to (see
//! [`crate::partition::Partition`]). It holds no visibility stamps and no dead
//! rows: [`crate::MemoryTier::demote`] encodes exactly the set visible at the
//! tier version it runs at, and writes never land on a cold partition — they
//! promote it back to a [`PredicatePartition`] first. So every row it holds is
//! visible at every version that can reach it, and the `_at` read methods can
//! ignore their version argument.
//!
//! # File format (little-endian)
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0  | 8 | magic [`MAGIC`] |
//! | 8  | 4 | format version ([`FORMAT_VERSION`]) |
//! | 12 | 8 | graph id bits |
//! | 20 | 8 | predicate id bits |
//! | 28 | 8 | row count |
//! | 36 | 8 | the tier commit version the rows were visible at |
//! | 44 | … | subject-major adjacency block |
//!
//! The adjacency block is [`crate::snapshot::format`]'s SPO block with the
//! predicate level removed (a partition has exactly one predicate) and global
//! [`TermId`] bits instead of per-snapshot dense local ids: `uvarint
//! num_subjects`, then per subject `uvarint gap(subject bits)`, `uvarint
//! num_objects`, and one `uvarint gap(object bits)` per object (gaps restart
//! at each subject).
//!
//! Only the subject-major block is stored. Object-major reads decode and sort
//! transiently — a second block would roughly double the file (SPEC-25 S5).

use crate::error::{Result, StorageError};
use crate::ordering::{Ordering, PartitionAxis};
use crate::partition::{OrderedColumns, PartitionBuilder, PredicatePartition};
use crate::snapshot::varint::{read_uvarint, write_uvarint};
use crate::term::{GraphId, TermId};
use crate::visibility::{CommitVersion, UNSET_END};
use arrow::array::UInt64Array;
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAGIC: [u8; 8] = *b"HDBCOLD\x01";
pub const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 44;

fn bad(msg: impl Into<String>) -> StorageError {
    StorageError::Snapshot(msg.into())
}

/// The file one predicate's cold partition lives in, under `cold_dir`.
pub fn cold_path(cold_dir: &Path, graph: GraphId, predicate: TermId) -> PathBuf {
    cold_dir.join(format!("{:016x}-{:016x}.cold", graph.0, predicate.0))
}

/// An immutable, memory-mapped partition. Cheap to clone the mapping; nothing
/// here allocates per row on a scan.
pub struct ColdPartition {
    mmap: Arc<Mmap>,
    path: PathBuf,
    rows: usize,
    version: CommitVersion,
    graph: GraphId,
    predicate: TermId,
}

impl std::fmt::Debug for ColdPartition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColdPartition")
            .field("path", &self.path)
            .field("rows", &self.rows)
            .field("version", &self.version)
            .finish()
    }
}

impl ColdPartition {
    /// Encode `rows` — which must arrive sorted subject-major and deduplicated,
    /// as [`PredicatePartition::scan_at`] yields them — into `path`, via a
    /// temporary file that is renamed into place. Returns the file length.
    pub fn write(
        path: &Path,
        graph: GraphId,
        predicate: TermId,
        version: CommitVersion,
        rows: impl Iterator<Item = (TermId, TermId)>,
    ) -> Result<u64> {
        let rows: Vec<(TermId, TermId)> = rows.collect();
        debug_assert!(
            rows.windows(2)
                .all(|w| (w[0].0 .0, w[0].1 .0) < (w[1].0 .0, w[1].1 .0)),
            "cold encoding requires strictly ascending subject-major rows"
        );
        let num_subjects = rows
            .windows(2)
            .filter(|w| w[0].0 != w[1].0)
            .count()
            .saturating_add(usize::from(!rows.is_empty()));

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut w = BufWriter::new(File::create(&tmp)?);
        w.write_all(&MAGIC)?;
        w.write_all(&FORMAT_VERSION.to_le_bytes())?;
        w.write_all(&graph.0.to_le_bytes())?;
        w.write_all(&predicate.0.to_le_bytes())?;
        w.write_all(&(rows.len() as u64).to_le_bytes())?;
        w.write_all(&version.to_le_bytes())?;

        write_uvarint(&mut w, num_subjects as u64)?;
        let mut i = 0usize;
        let mut prev_s = 0u64;
        while i < rows.len() {
            let s = rows[i].0 .0;
            write_uvarint(&mut w, s - prev_s)?;
            let start = i;
            while i < rows.len() && rows[i].0 .0 == s {
                i += 1;
            }
            write_uvarint(&mut w, (i - start) as u64)?;
            let mut prev_o = 0u64;
            for (_, o) in &rows[start..i] {
                write_uvarint(&mut w, o.0 - prev_o)?;
                prev_o = o.0;
            }
            prev_s = s;
        }
        let file = w.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        let len = file.metadata()?.len();
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(len)
    }

    /// Map `path` and validate it: header fields, then one full decode pass
    /// that must yield exactly the header's row count and consume every byte.
    /// Validating here is what lets [`Self::scan`] be infallible.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        // SAFETY: a cold file is only ever produced by `write`, which builds it
        // under a temporary name and renames it into place; nothing rewrites a
        // file at this path, so the mapping cannot observe a change underneath
        // it. Promotion unlinks the file, which leaves this inode (and this
        // mapping) intact.
        let mmap = Arc::new(unsafe { Mmap::map(&file)? });
        let bytes: &[u8] = &mmap;
        if bytes.len() < HEADER_LEN || bytes[..8] != MAGIC {
            return Err(bad(format!(
                "not a cold partition file: {}",
                path.display()
            )));
        }
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let version = u32_at(8);
        if version != FORMAT_VERSION {
            return Err(bad(format!("unsupported cold partition version {version}")));
        }
        let rows = u64_at(28);
        if rows > usize::MAX as u64 {
            return Err(bad(format!("cold partition row count {rows} out of range")));
        }
        let part = ColdPartition {
            path: path.to_path_buf(),
            rows: rows as usize,
            version: u64_at(36),
            graph: GraphId(u64_at(12)),
            predicate: TermId(u64_at(20)),
            mmap: mmap.clone(),
        };
        let mut scan = part.scan();
        let decoded = scan.by_ref().count();
        if decoded != part.rows || !scan.exhausted() {
            return Err(bad(format!(
                "cold partition row count mismatch: header {}, decoded {decoded}",
                part.rows
            )));
        }
        Ok(part)
    }

    /// The graph this partition belongs to.
    pub fn graph(&self) -> GraphId {
        self.graph
    }

    /// The predicate this partition holds.
    pub fn predicate(&self) -> TermId {
        self.predicate
    }

    /// The tier commit version the encoded rows were visible at. Every version
    /// that can reach this partition is `>= ` this one.
    pub fn version(&self) -> CommitVersion {
        self.version
    }

    /// The file this partition is mapped from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Mapped file length — a cold partition's whole footprint, since the rows
    /// live in the page cache rather than on the heap.
    pub fn mapped_bytes(&self) -> u64 {
        self.mmap.len() as u64
    }

    /// One forward decode pass over the mapped bytes, subject-major.
    pub fn scan(&self) -> ColdScan<'_> {
        ColdScan {
            cursor: &self.mmap[HEADER_LEN..],
            subjects_left: u64::MAX, // replaced by the header read below
            subject: 0,
            objects_left: 0,
            prev_o: 0,
            started: false,
        }
    }

    /// Is `(subject, object)` stored? Forward decode with an early exit once
    /// the subjects run past `subject`.
    ///
    /// ponytail: linear, because the file carries no per-subject offset index.
    /// Add one only if a bench shows cold point reads matter — the executor
    /// reads cold partitions through `ordered`, not this.
    pub fn contains(&self, subject: TermId, object: TermId) -> bool {
        for (s, o) in self.scan() {
            if s.0 > subject.0 {
                return false;
            }
            if s == subject && o == object {
                return true;
            }
        }
        false
    }

    /// Ordered access in any of the six orderings. Subject-major decodes
    /// straight into the two columns; object-major decodes and re-sorts, the
    /// same transient materialisation [`PredicatePartition::ordered_at`] does
    /// on its filtered branch.
    pub fn ordered(&self, ord: Ordering) -> OrderedColumns {
        let axis = ord.axis();
        let mut level0 = Vec::with_capacity(self.rows);
        let mut level1 = Vec::with_capacity(self.rows);
        match axis {
            PartitionAxis::SubjectMajor => {
                for (s, o) in self.scan() {
                    level0.push(s.0);
                    level1.push(o.0);
                }
            }
            PartitionAxis::ObjectMajor => {
                let mut rows: Vec<(u64, u64)> = self.scan().map(|(s, o)| (o.0, s.0)).collect();
                rows.sort_unstable();
                for (o, s) in rows {
                    level0.push(o);
                    level1.push(s);
                }
            }
        }
        OrderedColumns::new(
            axis,
            Arc::new(UInt64Array::from(level0)),
            Arc::new(UInt64Array::from(level1)),
        )
    }

    /// Rebuild a warm partition from these rows, all live from the version the
    /// file was encoded at.
    pub fn promote(&self, hot_threshold: usize) -> PredicatePartition {
        let mut b = PartitionBuilder::default();
        for (s, o) in self.scan() {
            b.append_stamped(s, o, self.version, UNSET_END);
        }
        b.build_with_hot_threshold(hot_threshold)
    }
}

/// Forward cursor over a cold partition's adjacency block.
///
/// Decode errors end the iteration rather than panicking: [`ColdPartition::open`]
/// has already walked the whole block, so a truncated or malformed file never
/// reaches here.
pub struct ColdScan<'a> {
    cursor: &'a [u8],
    subjects_left: u64,
    subject: u64,
    objects_left: u64,
    prev_o: u64,
    started: bool,
}

impl ColdScan<'_> {
    /// True once every byte of the block has been consumed — the other half of
    /// [`ColdPartition::open`]'s validation (a row count that matches but
    /// leaves trailing bytes is still a bad file).
    fn exhausted(&self) -> bool {
        self.cursor.is_empty() && self.subjects_left == 0 && self.objects_left == 0
    }
}

impl Iterator for ColdScan<'_> {
    type Item = (TermId, TermId);

    fn next(&mut self) -> Option<(TermId, TermId)> {
        if !self.started {
            self.started = true;
            self.subjects_left = read_uvarint(&mut self.cursor).ok()?;
        }
        while self.objects_left == 0 {
            if self.subjects_left == 0 {
                return None;
            }
            self.subjects_left -= 1;
            self.subject = self
                .subject
                .checked_add(read_uvarint(&mut self.cursor).ok()?)?;
            self.objects_left = read_uvarint(&mut self.cursor).ok()?;
            self.prev_o = 0;
        }
        self.objects_left -= 1;
        self.prev_o = self
            .prev_o
            .checked_add(read_uvarint(&mut self.cursor).ok()?)?;
        Some((TermId(self.subject), TermId(self.prev_o)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{TermKind, DEFAULT_GRAPH};

    fn id(payload: u64) -> TermId {
        TermId::new(TermKind::Uri, payload)
    }

    fn write_rows(dir: &Path, rows: &[(u64, u64)]) -> ColdPartition {
        let path = cold_path(dir, DEFAULT_GRAPH, id(100));
        ColdPartition::write(
            &path,
            DEFAULT_GRAPH,
            id(100),
            7,
            rows.iter().map(|&(s, o)| (id(s), id(o))),
        )
        .unwrap();
        ColdPartition::open(&path).unwrap()
    }

    #[test]
    fn round_trips_rows_and_header() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [(1u64, 2u64), (1, 5), (2, 9), (3, 1), (3, 2)];
        let cold = write_rows(dir.path(), &rows);
        assert_eq!(cold.len(), rows.len());
        assert_eq!(cold.version(), 7);
        assert_eq!(cold.graph(), DEFAULT_GRAPH);
        assert_eq!(cold.predicate(), id(100));
        let got: Vec<(u64, u64)> = cold
            .scan()
            .map(|(s, o)| (s.payload(), o.payload()))
            .collect();
        assert_eq!(got, rows);
    }

    #[test]
    fn empty_partition_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cold = write_rows(dir.path(), &[]);
        assert_eq!(cold.len(), 0);
        assert_eq!(cold.scan().count(), 0);
    }

    #[test]
    fn contains_finds_only_stored_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let cold = write_rows(dir.path(), &[(1, 2), (1, 5), (3, 1)]);
        assert!(cold.contains(id(1), id(2)));
        assert!(cold.contains(id(3), id(1)));
        assert!(!cold.contains(id(1), id(3)), "wrong object");
        assert!(!cold.contains(id(2), id(2)), "absent subject");
        assert!(!cold.contains(id(9), id(1)), "past the last subject");
    }

    #[test]
    fn open_rejects_a_bad_header() {
        let dir = tempfile::tempdir().unwrap();
        let cold = write_rows(dir.path(), &[(1, 2)]);
        let path = cold.path().to_path_buf();
        let mut bytes = std::fs::read(&path).unwrap();

        bytes[0] = b'X';
        std::fs::write(&path, &bytes).unwrap();
        let err = ColdPartition::open(&path).unwrap_err().to_string();
        assert!(err.contains("not a cold partition file"), "{err}");

        bytes[0] = b'H';
        bytes[8] = 9; // format version
        std::fs::write(&path, &bytes).unwrap();
        let err = ColdPartition::open(&path).unwrap_err().to_string();
        assert!(err.contains("unsupported cold partition version"), "{err}");

        bytes[8] = 1;
        bytes[28] = 42; // row count
        std::fs::write(&path, &bytes).unwrap();
        let err = ColdPartition::open(&path).unwrap_err().to_string();
        assert!(err.contains("row count mismatch"), "{err}");
    }

    #[test]
    fn promote_restores_every_row_live_at_the_encoded_version() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [(1u64, 2u64), (1, 5), (2, 9)];
        let warm = write_rows(dir.path(), &rows).promote(usize::MAX);
        assert_eq!(warm.len_at(7), rows.len());
        assert_eq!(warm.len_at(6), 0, "rows begin at the encoded version");
        assert!(!warm.has_retractions());
    }
}
