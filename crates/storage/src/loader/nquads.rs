//! Streaming N-Quads bulk loader.
//!
//! Uses `oxttl::NQuadsParser` to stream quads from any `Read` source, routing
//! each quad to the graph named by its fourth term (SPEC-02 F7) and batching
//! into the dictionary + tier in chunks of [`BATCH_SIZE`]. A quad with no graph
//! term lands in the default graph (the reserved [`DEFAULT_GRAPH`] sentinel);
//! a named (IRI or blank-node) graph term is interned and used as the graph id,
//! so triples with the same graph label co-locate.

use crate::error::{Result, StorageError};
use crate::loader::{subject_to_term, LoadStats, BATCH_SIZE};
use crate::store::Store;
use crate::term::{GraphId, DEFAULT_GRAPH};
use oxrdf::{GraphName, Term};
use oxttl::NQuadsParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

pub fn load_nquads_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_nquads_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_nquads_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let start = Instant::now();
    let parser = NQuadsParser::new();
    let iter = parser.for_reader(reader);

    let mut batch: Vec<(GraphId, _, _, _)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    for q in iter {
        let quad = q.map_err(|e| StorageError::NquadsParse(format!("{e}")))?;
        let g_id = graph_id(store, quad.graph_name)?;
        let s_term = subject_to_term(quad.subject);
        let p_term = Term::NamedNode(quad.predicate);
        let o_term = quad.object;

        let s_id = store.dictionary().intern(&s_term)?;
        let p_id = store.dictionary().intern(&p_term)?;
        let o_id = store.dictionary().intern(&o_term)?;
        batch.push((g_id, s_id, p_id, o_id));
        total += 1;
        if batch.len() >= BATCH_SIZE {
            store.tier().insert_quad_batch(&batch)?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        store.tier().insert_quad_batch(&batch)?;
    }

    Ok(LoadStats {
        triples: total,
        bytes_read: 0, // file caller will overwrite
        elapsed_ms: start.elapsed().as_millis() as u64,
        dictionary_size: store.dictionary().len() as u64,
    })
}

/// Map an N-Quads graph term to a [`GraphId`]. The default graph keeps the
/// reserved sentinel; a named (IRI) or blank-node graph label is interned via
/// the dictionary so identical labels collapse to the same id.
fn graph_id(store: &Store, g: GraphName) -> Result<GraphId> {
    match g {
        GraphName::DefaultGraph => Ok(DEFAULT_GRAPH),
        GraphName::NamedNode(n) => store.intern_graph_uri(&Term::NamedNode(n)),
        GraphName::BlankNode(b) => store.intern_graph_uri(&Term::BlankNode(b)),
    }
}
