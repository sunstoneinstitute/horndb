//! Write-ahead log + crash recovery (SPEC-25 S3, `PLAN-25-03`).
//!
//! One store directory holds:
//!
//! - `MANIFEST` — the current generation number, decimal. Written by temp file
//!   + `rename` + directory fsync; it is the checkpoint's commit point.
//! - `dict.<gen>` — the SPEC-25 S2 dictionary base flushed at the checkpoint
//!   (absent for generation 0, the empty store).
//! - `wal.<gen>` — records, each `[u32 body_len][u32 crc32c][body]`, little-
//!   endian. A generation above 0 opens with `Checkpoint` records (the rows
//!   visible at the checkpoint version); `Insert` / `Apply` records follow,
//!   one per committed batch, appended *before* the batch is applied.
//!
//! Body: `u8 kind`, `u64 version`, `u64 bnode_doc_tag`, `u64 dict_first`,
//! `u32 dict_count` × (`u32 len`, `term_codec` bytes), `u32 n_dels` × four
//! `u64`, `u32 n_adds` × four `u64`. The dictionary section carries every term
//! interned since the previous record, so no row ever names an id the log has
//! not spelled out (the S2 flush-vs-later-interns hazard). The kind byte
//! leaves room for SPEC-24 S5's `Input` / `TickCommit` records (ADR-0018).
//!
//! Tail discipline: a record cut short by the file end, or the last record
//! with a bad checksum, is a torn tail — dropped and truncated. A bad
//! checksum with bytes after it is [`StorageError::Wal`].

use crate::error::{Result, StorageError};
use crate::snapshot::term_codec;
use crate::term::{GraphId, TermId};
use oxrdf::Term;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) type Quad = (GraphId, TermId, TermId, TermId);

/// When an appended record reaches stable storage.
///
/// - `EveryBatch` (default): fsync after every record. A batch whose write
///   returned `Ok` survives a power loss.
/// - `Every(d)`: fsync on the first append at least `d` after the previous
///   fsync, and on [`Store::sync_wal`](crate::Store::sync_wal) / checkpoint.
///   Data-loss window on power loss: the records appended since the last
///   fsync. A process kill loses nothing under either policy (the bytes are
///   already in the kernel).
///
/// ponytail: no timer thread — a store that goes quiet under `Every(d)` stays
/// unsynced until its next append or an explicit sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncPolicy {
    #[default]
    EveryBatch,
    Every(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `Tier::insert_quad_batch` semantics: always mints a version.
    Insert = 1,
    /// `Tier::apply_quad_batch` semantics: mints a version iff net non-empty.
    Apply = 2,
    /// Rows visible at the checkpoint version; opens a generation.
    Checkpoint = 3,
}

pub(crate) struct Record {
    pub kind: Kind,
    pub version: u64,
    pub bnode_doc_tag: u64,
    /// Index of the first term in `dict`; the rest follow consecutively.
    pub dict_first: u64,
    pub dict: Vec<Term>,
    pub dels: Vec<Quad>,
    pub adds: Vec<Quad>,
}

fn wal_err(msg: impl Into<String>) -> StorageError {
    StorageError::Wal(msg.into())
}

// --- CRC-32C (Castagnoli), table-driven; no dependency carries one. ---------

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0x82F6_3B78 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c = CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

// --- record encoding ---------------------------------------------------------

fn put_u32(buf: &mut Vec<u8>, n: usize, what: &str) -> Result<()> {
    let n = u32::try_from(n).map_err(|_| wal_err(format!("{what}: {n} does not fit a record")))?;
    buf.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

fn put_quads(buf: &mut Vec<u8>, quads: &[Quad], what: &str) -> Result<()> {
    put_u32(buf, quads.len(), what)?;
    for (g, s, p, o) in quads {
        buf.extend_from_slice(&g.0.to_le_bytes());
        buf.extend_from_slice(&s.0.to_le_bytes());
        buf.extend_from_slice(&p.0.to_le_bytes());
        buf.extend_from_slice(&o.0.to_le_bytes());
    }
    Ok(())
}

pub(crate) fn encode(
    kind: Kind,
    version: u64,
    bnode_doc_tag: u64,
    dict_first: u64,
    dict: &[Term],
    dels: &[Quad],
    adds: &[Quad],
) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(64 + 32 * (dels.len() + adds.len()));
    buf.push(kind as u8);
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(&bnode_doc_tag.to_le_bytes());
    buf.extend_from_slice(&dict_first.to_le_bytes());
    put_u32(&mut buf, dict.len(), "dictionary appends")?;
    let mut term_buf = Vec::new();
    for term in dict {
        term_buf.clear();
        term_codec::encode_term(&mut term_buf, term);
        put_u32(&mut buf, term_buf.len(), "term length")?;
        buf.extend_from_slice(&term_buf);
    }
    put_quads(&mut buf, dels, "deletions")?;
    put_quads(&mut buf, adds, "insertions")?;
    Ok(buf)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(wal_err("record body shorter than its fields"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn quads(&mut self) -> Result<Vec<Quad>> {
        let n = self.u32()? as usize;
        let mut out = Vec::with_capacity(n.min(self.0.len() / 32));
        for _ in 0..n {
            out.push((
                GraphId(self.u64()?),
                TermId(self.u64()?),
                TermId(self.u64()?),
                TermId(self.u64()?),
            ));
        }
        Ok(out)
    }
}

pub(crate) fn decode(body: &[u8]) -> Result<Record> {
    let mut c = Cursor(body);
    let kind = match c.u8()? {
        1 => Kind::Insert,
        2 => Kind::Apply,
        3 => Kind::Checkpoint,
        k => return Err(wal_err(format!("unknown record kind {k}"))),
    };
    let version = c.u64()?;
    let bnode_doc_tag = c.u64()?;
    let dict_first = c.u64()?;
    let n_dict = c.u32()? as usize;
    let mut dict = Vec::with_capacity(n_dict.min(body.len()));
    for _ in 0..n_dict {
        let len = c.u32()? as usize;
        dict.push(term_codec::decode_term(c.take(len)?)?);
    }
    let dels = c.quads()?;
    let adds = c.quads()?;
    if !c.0.is_empty() {
        return Err(wal_err("trailing bytes in record body"));
    }
    Ok(Record {
        kind,
        version,
        bnode_doc_tag,
        dict_first,
        dict,
        dels,
        adds,
    })
}

// --- the log file ------------------------------------------------------------

const HEADER: usize = 8;

fn write_record(file: &mut File, body: &[u8]) -> Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| wal_err("record body over 4 GiB"))?;
    let mut frame = Vec::with_capacity(HEADER + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&crc32c(body).to_le_bytes());
    frame.extend_from_slice(body);
    file.write_all(&frame)?;
    Ok(())
}

fn fsync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub(crate) struct Wal {
    dir: PathBuf,
    gen: u64,
    file: File,
    /// Highest dictionary index a record has carried (or the base holds).
    pub(crate) logged_len: u64,
    policy: SyncPolicy,
    last_sync: Instant,
    dirty: bool,
}

impl Wal {
    pub(crate) fn dict_path(&self, gen: u64) -> PathBuf {
        self.dir.join(format!("dict.{gen}"))
    }

    fn wal_path(&self, gen: u64) -> PathBuf {
        self.dir.join(format!("wal.{gen}"))
    }

    pub(crate) fn generation(&self) -> u64 {
        self.gen
    }

    /// Open (or create) the log under `dir`. Returns the log and its
    /// generation; the caller opens `dict.<gen>` for `gen > 0` and then
    /// drives [`Wal::replay`]. Files of other generations — leftovers of a
    /// checkpoint that died before its `MANIFEST` rename — are removed.
    pub(crate) fn open(dir: &Path, policy: SyncPolicy) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let manifest = dir.join("MANIFEST");
        let gen = match fs::read_to_string(&manifest) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| wal_err(format!("MANIFEST: {e}")))?,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                write_manifest(dir, 0)?;
                0
            }
            Err(e) => return Err(e.into()),
        };
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let stale = ["dict.", "wal."].iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n != gen)
            });
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(dir.join(format!("wal.{gen}")))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            gen,
            file,
            logged_len: 0,
            policy,
            last_sync: Instant::now(),
            dirty: false,
        })
    }

    /// Read every complete record in order, handing each to `apply`, then
    /// truncate a torn tail. A checksum failure anywhere but the tail is an
    /// error.
    pub(crate) fn replay(&mut self, mut apply: impl FnMut(Record) -> Result<()>) -> Result<()> {
        let total = self.file.metadata()?.len();
        let mut reader = BufReader::new(&self.file);
        let mut good_end = 0u64;
        let mut header = [0u8; HEADER];
        let mut body = Vec::new();
        loop {
            let remaining = total - good_end;
            if remaining < HEADER as u64 {
                break; // clean end (0) or a torn header
            }
            reader.read_exact(&mut header)?;
            let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as u64;
            let crc = u32::from_le_bytes(header[4..].try_into().unwrap());
            if remaining - (HEADER as u64) < len {
                break; // torn body
            }
            body.clear();
            body.resize(len as usize, 0);
            reader.read_exact(&mut body)?;
            let end = good_end + HEADER as u64 + len;
            if crc32c(&body) != crc {
                if end == total {
                    break; // torn tail: length landed, body did not
                }
                return Err(wal_err(format!(
                    "checksum mismatch in record at byte {good_end} of wal.{}",
                    self.gen
                )));
            }
            apply(decode(&body)?)?;
            good_end = end;
        }
        if good_end < total {
            self.file.set_len(good_end)?;
            self.file.sync_all()?;
        }
        Ok(())
    }

    pub(crate) fn append(&mut self, body: &[u8]) -> Result<()> {
        write_record(&mut self.file, body)?;
        self.dirty = true;
        match self.policy {
            SyncPolicy::EveryBatch => self.sync(),
            SyncPolicy::Every(d) if self.last_sync.elapsed() >= d => self.sync(),
            SyncPolicy::Every(_) => Ok(()),
        }
    }

    pub(crate) fn sync(&mut self) -> Result<()> {
        if self.dirty {
            self.file.sync_data()?;
            self.dirty = false;
        }
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Start generation `gen`: a fresh `wal.<gen>` the caller fills with
    /// `Checkpoint` records via [`Wal::write_checkpoint_record`], then commits
    /// with [`Wal::commit_generation`]. A leftover file from an earlier failed
    /// attempt is overwritten.
    pub(crate) fn start_generation(&self, gen: u64) -> Result<File> {
        Ok(File::create(self.wal_path(gen))?)
    }

    pub(crate) fn write_checkpoint_record(
        file: &mut File,
        version: u64,
        bnode_doc_tag: u64,
        rows: &[Quad],
    ) -> Result<()> {
        write_record(
            file,
            &encode(Kind::Checkpoint, version, bnode_doc_tag, 0, &[], &[], rows)?,
        )
    }

    /// Make `gen` current: fsync its log, point `MANIFEST` at it, drop the
    /// previous generation's files, and continue appending to the new log.
    /// `logged_len` is the slot count of `dict.<gen>`.
    pub(crate) fn commit_generation(
        &mut self,
        gen: u64,
        file: File,
        logged_len: u64,
    ) -> Result<()> {
        file.sync_all()?;
        write_manifest(&self.dir, gen)?;
        let old = self.gen;
        let _ = fs::remove_file(self.wal_path(old));
        if old > 0 {
            let _ = fs::remove_file(self.dict_path(old));
        }
        self.gen = gen;
        self.file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(self.wal_path(gen))?;
        drop(file);
        self.logged_len = logged_len;
        self.dirty = false;
        self.last_sync = Instant::now();
        Ok(())
    }
}

fn write_manifest(dir: &Path, gen: u64) -> Result<()> {
    let tmp = dir.join(format!("MANIFEST.{}.tmp", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(format!("{gen}\n").as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join("MANIFEST"))?;
    fsync_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn record_round_trips() {
        let term = Term::NamedNode(oxrdf::NamedNode::new("http://ex/a").unwrap());
        let q = (GraphId(0), TermId(1), TermId(2), TermId(3));
        let body = encode(
            Kind::Apply,
            7,
            2,
            5,
            std::slice::from_ref(&term),
            &[q],
            &[q, q],
        )
        .unwrap();
        let rec = decode(&body).unwrap();
        assert_eq!(rec.kind, Kind::Apply);
        assert_eq!((rec.version, rec.bnode_doc_tag, rec.dict_first), (7, 2, 5));
        assert_eq!(rec.dict, vec![term]);
        assert_eq!(rec.dels, vec![q]);
        assert_eq!(rec.adds, vec![q, q]);
        assert!(decode(&body[..body.len() - 1]).is_err());
    }
}
