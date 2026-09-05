//! HDT-derived compact snapshot format (SPEC-02 F9).
//!
//! Exports every graph of a [`Store`] — the default graph plus all named
//! graphs — to a compact byte stream and re-imports it (SPEC-25 S4).
//! **Not** wire-compatible with the rdfhdt reference format; cross-tool
//! interop is out of scope.
//!
//! A store with no named-graph data still writes the Stage-1 v1 layout;
//! named-graph data bumps the version to v2, which Stage-1 readers reject
//! through the `unsupported snapshot version` path.
//!
//! Format spec: docs/plans/PLAN-02-02-hdt-snapshot.md.

pub mod format;
pub mod term_codec;
pub mod varint;

use crate::error::{Result, StorageError};
use crate::store::Store;
use crate::term::{TermId, TermKind, DEFAULT_GRAPH};
use format::{GraphBlock, LocalTriple};
use std::collections::HashMap;
use std::io::{Read, Write};

/// Byte accounting for an exported snapshot (drives the NF1 footprint check).
#[derive(Debug, Clone, Copy)]
pub struct SnapshotStats {
    /// Total quads across every graph in the snapshot.
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

/// Export every graph of `store` to `w` in the snapshot format.
pub fn export_snapshot<W: Write>(store: &Store, w: &mut W) -> Result<SnapshotStats> {
    // Pin one snapshot for the whole export: enumerating the graphs and
    // scanning each of them must observe the SAME store state, or a write
    // landing in between could produce an internally inconsistent checkpoint.
    let snap = store.snapshot();
    let graph_ids = snap.graphs();
    let mut raw = Vec::with_capacity(graph_ids.len());
    for g in graph_ids {
        raw.push((g, snap.iter_graph_term_ids(g).collect::<Vec<_>>()));
    }

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
            let term = snap
                .dictionary()
                .lookup(id)
                .ok_or_else(|| StorageError::Snapshot(format!("dangling term id {id:?}")))?;
            term_codec::encode_term(&mut buf, &term);
        }
        enc_by_id.insert(id, buf);
        Ok(())
    };
    for (g, triples) in &raw {
        if *g != DEFAULT_GRAPH {
            encode(TermId(g.0))?; // the graph name is part of the dictionary
        }
        for (s, p, o) in triples {
            encode(*s)?;
            encode(*p)?;
            encode(*o)?;
        }
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

    let total_quads: u64 = raw.iter().map(|(_, t)| t.len() as u64).sum();
    let mut graphs: Vec<GraphBlock> = raw
        .iter()
        .map(|(g, triples)| GraphBlock {
            // Local id 0 is reserved for the default graph, which has no name.
            graph_local: if *g == DEFAULT_GRAPH {
                0
            } else {
                local_of[&TermId(g.0)]
            },
            triples: triples
                .iter()
                .map(|(s, p, o)| LocalTriple {
                    s: local_of[s],
                    p: local_of[p],
                    o: local_of[o],
                })
                .collect(),
        })
        .collect();

    let (dict_bytes, tri_bytes) = format::write_snapshot(w, &terms, &mut graphs)?;
    Ok(SnapshotStats {
        triples: total_quads,
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

/// Import a snapshot from `r`, inserting its quads into `store`.
pub fn import_snapshot_into<R: Read>(store: &Store, r: &mut R) -> Result<()> {
    let (term_bytes, graphs) = format::read_snapshot(r)?;
    // Decode terms (local id = index + 1).
    let mut terms = Vec::with_capacity(term_bytes.len());
    for bytes in &term_bytes {
        terms.push(term_codec::decode_term(bytes)?);
    }
    // Resolve a 1-based local id to its decoded term (cloned into the batch).
    let resolve = |local: u64, position: &str| {
        terms
            .get((local.wrapping_sub(1)) as usize)
            .cloned()
            .ok_or_else(|| StorageError::Snapshot(format!("{position} local id out of range")))
    };
    for block in &graphs {
        if block.graph_local == 0 {
            let mut batch = Vec::with_capacity(block.triples.len());
            for t in &block.triples {
                batch.push((
                    resolve(t.s, "subject")?,
                    resolve(t.p, "predicate")?,
                    resolve(t.o, "object")?,
                ));
            }
            store.insert_triples(&batch)?;
        } else {
            let g = store.intern_graph_uri(&resolve(block.graph_local, "graph")?)?;
            let mut batch = Vec::with_capacity(block.triples.len());
            for t in &block.triples {
                batch.push((
                    g,
                    resolve(t.s, "subject")?,
                    resolve(t.p, "predicate")?,
                    resolve(t.o, "object")?,
                ));
            }
            store.insert_quads(&batch)?;
        }
    }
    Ok(())
}
