//! Streaming N-Triples bulk loader.
//!
//! Uses `oxttl::NTriplesParser` to stream triples from any `Read` source,
//! batching into the dictionary + tier in chunks of [`BATCH_SIZE`].

use crate::error::{Result, StorageError};
use crate::loader::{load_quads, subject_to_term};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::Term;
use oxttl::NTriplesParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub use crate::loader::LoadStats;

pub fn load_ntriples_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_ntriples_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_ntriples_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let parser = NTriplesParser::new();
    load_quads(
        store,
        parser.for_reader(reader).map(|t| {
            let triple = t.map_err(|e| StorageError::NtriplesParse(format!("{e}")))?;
            Ok((
                DEFAULT_GRAPH,
                subject_to_term(triple.subject),
                Term::NamedNode(triple.predicate),
                triple.object,
            ))
        }),
    )
}
