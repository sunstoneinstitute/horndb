//! Streaming N-Triples bulk loader.
//!
//! Uses `oxttl::NTriplesParser` to stream triples from any `Read` source,
//! batching into the dictionary + tier in chunks of `BATCH_SIZE`.

use crate::error::{Result, StorageError};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::{Subject, Term};
use oxttl::NTriplesParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;

const BATCH_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct LoadStats {
    pub triples: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub dictionary_size: u64,
}

pub fn load_ntriples_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stats = load_ntriples_reader(store, reader)?;
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_ntriples_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let start = Instant::now();
    let parser = NTriplesParser::new();
    let iter = parser.for_reader(reader);

    let mut batch: Vec<(crate::term::GraphId, _, _, _)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    // Pre-intern terms via dictionary, then push encoded quad into batch.
    for t in iter {
        let triple = t.map_err(|e| StorageError::NtriplesParse(format!("{e}")))?;
        // Convert oxrdf::Subject and oxrdf::Term-style nodes into oxrdf::Term.
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

fn subject_to_term(s: Subject) -> Term {
    match s {
        Subject::NamedNode(n) => Term::NamedNode(n),
        Subject::BlankNode(b) => Term::BlankNode(b),
        // oxrdf gates triples-as-subjects behind the `rdf-star` feature, which
        // we do not enable; this arm is therefore unreachable in Stage 1.
        #[allow(unreachable_patterns)]
        _ => panic!("RDF-star subject not supported in Stage 1"),
    }
}
