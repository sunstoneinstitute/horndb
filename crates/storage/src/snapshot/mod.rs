//! HDT-derived compact snapshot format (SPEC-02 F9).
//!
//! Exports the default graph of a [`Store`] to a compact byte stream and
//! re-imports it. **Not** wire-compatible with the rdfhdt reference format;
//! cross-tool interop is out of scope for this increment. Named-graph snapshots
//! are a documented follow-up — [`export_snapshot`] errors if the store holds
//! named-graph data rather than silently dropping it.
//!
//! Format spec: docs/plans/2026-06-14-SPEC-02-hdt-snapshot.md.

pub mod format;
pub mod term_codec;
pub mod varint;

use crate::error::{Result, StorageError};
use crate::store::Store;
use crate::term::{TermId, TermKind};
use format::LocalTriple;
use std::collections::HashMap;
use std::io::{Read, Write};

/// Byte accounting for an exported snapshot (drives the NF1 footprint check).
#[derive(Debug, Clone, Copy)]
pub struct SnapshotStats {
    pub triples: u64,
    pub distinct_terms: u64,
    pub dictionary_bytes: u64,
    pub triples_bytes: u64,
    pub total_bytes: u64,
}

impl SnapshotStats {
    pub fn bytes_per_triple(&self) -> f64 {
        if self.triples == 0 {
            0.0
        } else {
            self.total_bytes as f64 / self.triples as f64
        }
    }
}

/// Export the default graph of `store` to `w` in the snapshot format.
pub fn export_snapshot<W: Write>(store: &Store, w: &mut W) -> Result<SnapshotStats> {
    if store.has_named_graph_data() {
        return Err(StorageError::Snapshot(
            "named-graph snapshot export not yet supported (default graph only)".into(),
        ));
    }
    let raw = store.scan_all_term_ids();

    // Collect distinct term ids and their canonical encodings.
    let mut enc_by_id: HashMap<TermId, Vec<u8>> = HashMap::new();
    let mut encode = |id: TermId| -> Result<()> {
        if enc_by_id.contains_key(&id) {
            return Ok(());
        }
        let mut buf = Vec::new();
        if id.kind() == TermKind::InlineInt {
            let v = id.as_inline_int().expect("inline int id");
            term_codec::encode_inline_int(&mut buf, v);
        } else {
            let term = store
                .dictionary()
                .lookup(id)
                .ok_or_else(|| StorageError::Snapshot(format!("dangling term id {id:?}")))?;
            term_codec::encode_term(&mut buf, &term);
        }
        enc_by_id.insert(id, buf);
        Ok(())
    };
    for (s, p, o) in &raw {
        encode(*s)?;
        encode(*p)?;
        encode(*o)?;
    }

    // Sort distinct encodings, assign dense local ids (1-based).
    let mut entries: Vec<(TermId, Vec<u8>)> = enc_by_id.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    let mut local_of: HashMap<TermId, u64> = HashMap::with_capacity(entries.len());
    let mut terms: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
    for (i, (id, bytes)) in entries.into_iter().enumerate() {
        local_of.insert(id, (i + 1) as u64);
        terms.push(bytes);
    }

    let mut triples: Vec<LocalTriple> = raw
        .iter()
        .map(|(s, p, o)| LocalTriple {
            s: local_of[s],
            p: local_of[p],
            o: local_of[o],
        })
        .collect();

    let (dict_bytes, tri_bytes) = format::write_snapshot(w, &terms, &mut triples)?;
    Ok(SnapshotStats {
        triples: raw.len() as u64,
        distinct_terms: terms.len() as u64,
        dictionary_bytes: dict_bytes,
        triples_bytes: tri_bytes,
        total_bytes: 32 + dict_bytes + tri_bytes, // 32-byte header
    })
}

/// Import a snapshot from `r` into a fresh in-memory [`Store`].
pub fn import_snapshot<R: Read>(r: &mut R) -> Result<Store> {
    let store = Store::in_memory();
    import_snapshot_into(&store, r)?;
    Ok(store)
}

/// Import a snapshot from `r`, inserting its default-graph triples into `store`.
pub fn import_snapshot_into<R: Read>(store: &Store, r: &mut R) -> Result<()> {
    let (term_bytes, triples) = format::read_snapshot(r)?;
    // Decode terms (local id = index + 1).
    let mut terms = Vec::with_capacity(term_bytes.len());
    for bytes in &term_bytes {
        terms.push(term_codec::decode_term(bytes)?);
    }
    let mut batch = Vec::with_capacity(triples.len());
    for t in &triples {
        let s = terms
            .get((t.s - 1) as usize)
            .ok_or_else(|| StorageError::Snapshot("subject local id out of range".into()))?;
        let p = terms
            .get((t.p - 1) as usize)
            .ok_or_else(|| StorageError::Snapshot("predicate local id out of range".into()))?;
        let o = terms
            .get((t.o - 1) as usize)
            .ok_or_else(|| StorageError::Snapshot("object local id out of range".into()))?;
        batch.push((s.clone(), p.clone(), o.clone()));
    }
    store.insert_triples(&batch)?;
    Ok(())
}
