//! Streaming Turtle bulk loader.
//!
//! Uses `oxttl::TurtleParser` to stream triples from any `Read` source,
//! batching into the dictionary + tier in chunks of [`BATCH_SIZE`]. Turtle
//! carries no graph component, so every triple lands in the default graph
//! (SPEC-02 F7's reserved sentinel). Prefixes, the base IRI, `a`, collections,
//! and blank-node property lists are all expanded by the parser before they
//! reach the dictionary.

use crate::error::{Result, StorageError};
use crate::loader::{subject_to_term, LoadStats, BATCH_SIZE};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::Term;
use oxttl::TurtleParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

pub fn load_turtle_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_turtle_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_turtle_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let start = Instant::now();
    let parser = TurtleParser::new();
    let iter = parser.for_reader(reader);

    let mut batch: Vec<(crate::term::GraphId, _, _, _)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    for t in iter {
        let triple = t.map_err(|e| StorageError::TurtleParse(format!("{e}")))?;
        let s_term = subject_to_term(triple.subject);
        let p_term = Term::NamedNode(triple.predicate);
        let o_term = triple.object;

        let s_id = store.dictionary().intern(&s_term)?;
        let p_id = store.dictionary().intern(&p_term)?;
        let o_id = store.dictionary().intern(&o_term)?;
        batch.push((DEFAULT_GRAPH, s_id, p_id, o_id));
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
