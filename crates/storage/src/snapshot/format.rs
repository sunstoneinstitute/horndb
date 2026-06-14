//! Byte-level reader/writer for the HDT-derived snapshot format.
//!
//! Layout is specified in docs/plans/2026-06-14-SPEC-02-hdt-snapshot.md.

use super::varint::{read_uvarint, write_uvarint};
use crate::error::{Result, StorageError};
use std::io::{Read, Write};

pub const MAGIC: [u8; 8] = *b"HDBSNAP\x01";
pub const FORMAT_VERSION: u32 = 1;

/// Upper bound on elements pre-reserved from untrusted header counts, so a
/// crafted header can't trigger a huge allocation before any data is read.
const MAX_PREALLOC: usize = 1 << 20;

fn snap_err(msg: impl Into<String>) -> StorageError {
    StorageError::Snapshot(msg.into())
}

/// A triple in dense local-id space (1-based ids into the dictionary).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LocalTriple {
    pub s: u64,
    pub p: u64,
    pub o: u64,
}

/// Write the full snapshot. `terms` are canonical encodings already sorted
/// ascending (local id = index + 1). `triples` are in local-id space and will be
/// sorted SPO here. Returns (dictionary_bytes, triples_bytes) actually written.
pub fn write_snapshot<W: Write>(
    w: &mut W,
    terms: &[Vec<u8>],
    triples: &mut [LocalTriple],
) -> Result<(u64, u64)> {
    // Header.
    w.write_all(&MAGIC).map_err(StorageError::from)?;
    w.write_all(&FORMAT_VERSION.to_le_bytes())
        .map_err(StorageError::from)?;
    w.write_all(&0u32.to_le_bytes())
        .map_err(StorageError::from)?; // flags
    w.write_all(&(terms.len() as u64).to_le_bytes())
        .map_err(StorageError::from)?;
    w.write_all(&(triples.len() as u64).to_le_bytes())
        .map_err(StorageError::from)?;

    // Dictionary (front-coded).
    let mut dict_buf = Vec::new();
    let mut prev: &[u8] = &[];
    for cur in terms {
        let shared = common_prefix_len(prev, cur);
        write_uvarint(&mut dict_buf, shared as u64).map_err(StorageError::from)?;
        write_uvarint(&mut dict_buf, (cur.len() - shared) as u64).map_err(StorageError::from)?;
        dict_buf.extend_from_slice(&cur[shared..]);
        prev = cur;
    }
    w.write_all(&dict_buf).map_err(StorageError::from)?;

    // Triples (SPO adjacency, gap-coded).
    triples.sort_unstable_by(|a, b| (a.s, a.p, a.o).cmp(&(b.s, b.p, b.o)));
    let mut tri_buf = Vec::new();
    write_adjacency(&mut tri_buf, triples)?;
    w.write_all(&tri_buf).map_err(StorageError::from)?;

    Ok((dict_buf.len() as u64, tri_buf.len() as u64))
}

/// Read a full snapshot, returning the decoded term-encoding byte strings (in
/// local-id order) and the triples in local-id space.
pub fn read_snapshot<R: Read>(r: &mut R) -> Result<(Vec<Vec<u8>>, Vec<LocalTriple>)> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).map_err(StorageError::from)?;
    if magic != MAGIC {
        return Err(snap_err("bad magic / not a horndb snapshot"));
    }
    let version = read_u32(r)?;
    if version != FORMAT_VERSION {
        return Err(snap_err(format!("unsupported snapshot version {version}")));
    }
    let _flags = read_u32(r)?;
    let num_terms = read_u64(r)?;
    let num_triples = read_u64(r)?;

    // Dictionary.
    let mut terms: Vec<Vec<u8>> = Vec::with_capacity((num_terms as usize).min(MAX_PREALLOC));
    let mut prev: Vec<u8> = Vec::new();
    for _ in 0..num_terms {
        let shared = read_uvarint(r).map_err(StorageError::from)? as usize;
        let suffix_len = read_uvarint(r).map_err(StorageError::from)? as usize;
        if shared > prev.len() {
            return Err(snap_err("front-coding shared prefix exceeds previous term"));
        }
        // Reserve only the bounded part eagerly; `suffix_len` is untrusted, so
        // capping the reservation keeps a crafted header from triggering a huge
        // allocation before any suffix bytes are read.
        let mut cur = Vec::with_capacity(shared + suffix_len.min(MAX_PREALLOC));
        cur.extend_from_slice(&prev[..shared]);
        // Read at most `suffix_len` bytes via a bounded, geometrically-growing
        // read. A crafted stream declaring a huge `suffix_len` only allocates the
        // bytes that actually arrive (then we error), instead of pre-allocating
        // `suffix_len` eagerly and risking an OOM/abort.
        let mut suffix = Vec::new();
        let got = r
            .by_ref()
            .take(suffix_len as u64)
            .read_to_end(&mut suffix)
            .map_err(StorageError::from)?;
        if got != suffix_len {
            return Err(snap_err("dictionary suffix truncated"));
        }
        cur.extend_from_slice(&suffix);
        prev = cur.clone();
        terms.push(cur);
    }

    // Triples.
    let triples = read_adjacency(r, num_terms, num_triples)?;
    Ok((terms, triples))
}

/// Write the SPO adjacency (gap-coded) for `sorted`.
///
/// Precondition: `sorted` must be sorted ascending by `(s, p, o)`. The dedup of
/// distinct subjects/predicates below is only correct for sorted input.
fn write_adjacency<W: Write>(w: &mut W, sorted: &[LocalTriple]) -> Result<()> {
    debug_assert!(
        sorted
            .windows(2)
            .all(|w| (w[0].s, w[0].p, w[0].o) <= (w[1].s, w[1].p, w[1].o)),
        "write_adjacency requires SPO-sorted input"
    );
    // Count distinct subjects.
    let subjects: Vec<u64> = {
        let mut v: Vec<u64> = sorted.iter().map(|t| t.s).collect();
        v.dedup();
        v
    };
    write_uvarint(w, subjects.len() as u64).map_err(StorageError::from)?;
    let mut i = 0usize;
    let mut prev_s = 0u64;
    while i < sorted.len() {
        let s = sorted[i].s;
        write_uvarint(w, s - prev_s).map_err(StorageError::from)?;
        // gather the slice for this subject
        let s_start = i;
        while i < sorted.len() && sorted[i].s == s {
            i += 1;
        }
        let s_slice = &sorted[s_start..i];
        // distinct predicates
        let mut preds: Vec<u64> = s_slice.iter().map(|t| t.p).collect();
        preds.dedup();
        write_uvarint(w, preds.len() as u64).map_err(StorageError::from)?;
        let mut j = 0usize;
        let mut prev_p = 0u64;
        while j < s_slice.len() {
            let p = s_slice[j].p;
            write_uvarint(w, p - prev_p).map_err(StorageError::from)?;
            let p_start = j;
            while j < s_slice.len() && s_slice[j].p == p {
                j += 1;
            }
            let objs = &s_slice[p_start..j];
            write_uvarint(w, objs.len() as u64).map_err(StorageError::from)?;
            let mut prev_o = 0u64;
            for t in objs {
                write_uvarint(w, t.o - prev_o).map_err(StorageError::from)?;
                prev_o = t.o;
            }
            prev_p = p;
        }
        prev_s = s;
    }
    Ok(())
}

fn read_adjacency<R: Read>(
    r: &mut R,
    num_terms: u64,
    num_triples: u64,
) -> Result<Vec<LocalTriple>> {
    let mut out = Vec::with_capacity((num_triples as usize).min(MAX_PREALLOC));
    let num_subjects = read_uvarint(r).map_err(StorageError::from)?;
    let mut prev_s = 0u64;
    for _ in 0..num_subjects {
        let s = prev_s
            .checked_add(read_uvarint(r).map_err(StorageError::from)?)
            .ok_or_else(|| snap_err("local-id gap overflow"))?;
        let num_preds = read_uvarint(r).map_err(StorageError::from)?;
        let mut prev_p = 0u64;
        for _ in 0..num_preds {
            let p = prev_p
                .checked_add(read_uvarint(r).map_err(StorageError::from)?)
                .ok_or_else(|| snap_err("local-id gap overflow"))?;
            let num_objs = read_uvarint(r).map_err(StorageError::from)?;
            let mut prev_o = 0u64;
            for _ in 0..num_objs {
                let o = prev_o
                    .checked_add(read_uvarint(r).map_err(StorageError::from)?)
                    .ok_or_else(|| snap_err("local-id gap overflow"))?;
                if s == 0 || p == 0 || o == 0 || s > num_terms || p > num_terms || o > num_terms {
                    return Err(snap_err("triple references out-of-range local id"));
                }
                out.push(LocalTriple { s, p, o });
                prev_o = o;
            }
            prev_p = p;
        }
        prev_s = s;
    }
    if out.len() as u64 != num_triples {
        return Err(snap_err(format!(
            "triple count mismatch: header {num_triples}, decoded {}",
            out.len()
        )));
    }
    Ok(out)
}

fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(StorageError::from)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(StorageError::from)?;
    Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_bytes_round_trip() {
        let terms = vec![
            b"\x00http://ex/a".to_vec(),
            b"\x00http://ex/b".to_vec(),
            b"\x00http://ex/p".to_vec(),
        ];
        let mut triples = vec![
            LocalTriple { s: 1, p: 3, o: 2 },
            LocalTriple { s: 1, p: 3, o: 1 },
        ];
        let mut buf = Vec::new();
        write_snapshot(&mut buf, &terms, &mut triples).unwrap();
        let (rt_terms, mut rt_triples) = read_snapshot(&mut &buf[..]).unwrap();
        assert_eq!(rt_terms, terms);
        rt_triples.sort_unstable_by(|a, b| (a.s, a.p, a.o).cmp(&(b.s, b.p, b.o)));
        assert_eq!(
            rt_triples,
            vec![
                LocalTriple { s: 1, p: 3, o: 1 },
                LocalTriple { s: 1, p: 3, o: 2 },
            ]
        );
    }

    #[test]
    fn front_coding_shares_prefixes() {
        // Two long shared-prefix URIs produce a small dictionary section.
        let terms = vec![
            b"\x00http://example.org/very/long/path/aaaa".to_vec(),
            b"\x00http://example.org/very/long/path/aaab".to_vec(),
        ];
        let mut triples: Vec<LocalTriple> = vec![];
        let mut buf = Vec::new();
        let (dict_bytes, _) = write_snapshot(&mut buf, &terms, &mut triples).unwrap();
        // Second entry should only cost its 1-byte suffix + 2 varint lengths.
        assert!(
            dict_bytes < (terms[0].len() + 8) as u64,
            "dict not front-coded: {dict_bytes}"
        );
    }

    #[test]
    fn bad_magic_errors() {
        let buf = [0u8; 32];
        assert!(read_snapshot(&mut &buf[..]).is_err());
    }

    #[test]
    fn out_of_range_local_id_errors() {
        // Craft a stream whose only term yields local ids 1..=1, but whose triple
        // section references local id 2 (> num_terms). The read path must reject it.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags
        buf.extend_from_slice(&1u64.to_le_bytes()); // num_terms = 1
        buf.extend_from_slice(&1u64.to_le_bytes()); // num_triples = 1
                                                    // Dictionary: one term, front-coded against empty prev.
        let term: &[u8] = b"\x00http://ex/a";
        write_uvarint(&mut buf, 0).unwrap(); // shared_prefix_len
        write_uvarint(&mut buf, term.len() as u64).unwrap(); // suffix_len
        buf.extend_from_slice(term);
        // Triples: 1 subject, s_gap=1 (s=1), 1 pred, p_gap=1 (p=1), 1 obj,
        // o_gap=2 (o=2) -> o=2 exceeds num_terms=1.
        write_uvarint(&mut buf, 1).unwrap(); // num_subjects
        write_uvarint(&mut buf, 1).unwrap(); // s_gap -> s = 1
        write_uvarint(&mut buf, 1).unwrap(); // num_preds
        write_uvarint(&mut buf, 1).unwrap(); // p_gap -> p = 1
        write_uvarint(&mut buf, 1).unwrap(); // num_objs
        write_uvarint(&mut buf, 2).unwrap(); // o_gap -> o = 2 (out of range)

        let err = read_snapshot(&mut &buf[..]);
        assert!(err.is_err(), "expected out-of-range local id to error");
    }

    #[test]
    fn read_snapshot_rejects_absurd_header() {
        // Hand-craft a 32-byte header claiming u64::MAX terms but with no body.
        // The reader must NOT panic/abort on the huge with_capacity; it should
        // fail cleanly once read_exact runs out of dictionary bytes.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags
        buf.extend_from_slice(&u64::MAX.to_le_bytes()); // num_terms = absurd
        buf.extend_from_slice(&u64::MAX.to_le_bytes()); // num_triples = absurd
        assert_eq!(buf.len(), 32);
        // No body follows.
        let err = read_snapshot(&mut &buf[..]);
        assert!(err.is_err(), "expected absurd header to error, not panic");
    }

    #[test]
    fn read_snapshot_rejects_huge_suffix_len() {
        // Header claims a single term, then the dictionary entry declares an
        // enormous suffix_len but supplies only a couple of bytes. The reader
        // must fail cleanly (bounded read) instead of eagerly allocating
        // suffix_len bytes and panicking/aborting.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags
        buf.extend_from_slice(&1u64.to_le_bytes()); // num_terms = 1
        buf.extend_from_slice(&0u64.to_le_bytes()); // num_triples = 0
        assert_eq!(buf.len(), 32);
        // Dictionary entry: shared_prefix_len = 0, suffix_len = absurd.
        write_uvarint(&mut buf, 0).unwrap(); // shared_prefix_len
        write_uvarint(&mut buf, u64::MAX).unwrap(); // suffix_len = absurd
        buf.extend_from_slice(&[0xAB, 0xCD]); // only two bytes of "suffix"

        let err = read_snapshot(&mut &buf[..]);
        assert!(
            err.is_err(),
            "expected huge suffix_len to error, not panic/abort"
        );
    }
}
