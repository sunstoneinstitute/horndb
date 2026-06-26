//! Streaming N-Quads bulk loader.
//!
//! Uses `oxttl::NQuadsParser` to stream quads from any `Read` source, routing
//! each quad to the graph named by its fourth term (SPEC-02 F7) and batching
//! into the dictionary + tier in chunks of [`BATCH_SIZE`]. A quad with no graph
//! term lands in the default graph (the reserved [`DEFAULT_GRAPH`] sentinel);
//! a named (IRI or blank-node) graph term is interned and used as the graph id,
//! so triples with the same graph label co-locate.

use crate::error::{Result, StorageError};
use crate::loader::{load_quads, subject_to_term, LoadStats};
use crate::store::Store;
use crate::term::{GraphId, DEFAULT_GRAPH};
use oxrdf::{GraphName, Term};
use oxttl::NQuadsParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub fn load_nquads_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_nquads_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_nquads_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let parser = NQuadsParser::new();
    load_quads(
        store,
        parser.for_reader(reader).map(|q| {
            let quad = q.map_err(|e| StorageError::NquadsParse(format!("{e}")))?;
            let g_id = graph_id(store, quad.graph_name)?;
            Ok((
                g_id,
                subject_to_term(quad.subject),
                Term::NamedNode(quad.predicate),
                quad.object,
            ))
        }),
    )
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
