//! N-Triples bulk loader — streaming and parallel-chunked.
//!
//! Two entry points feed the same intern + batch path
//! ([`crate::loader::QuadSink`]):
//!
//! * [`load_ntriples_reader`] streams from any `Read` on one thread. This is
//!   the only option for a non-seekable source (HTTP body, stdin).
//! * [`load_ntriples_slice`] parses an in-memory document on
//!   [`load_threads`] threads via `oxttl`'s `split_slice_for_parallel_parsing`.
//!
//! Splitting N-Triples is unconditionally safe: the chunker cuts at newlines,
//! and the grammar forbids a raw newline inside a literal, an IRI, or a blank
//! node label — so every newline is a statement boundary. Blank node labels
//! keep document scope because `oxttl` emits a labelled blank node verbatim
//! (`BlankNode::new_unchecked(label)`), so `_:b1` denotes the same node in
//! every chunk.

use crate::dictionary::Dictionary;
use crate::error::{Result, StorageError};
use crate::loader::parallel::{
    load_threads, parse_chunks_mapped, parse_chunks_ordered, should_probe, should_read_whole_file,
    slice_threads,
};
use crate::loader::{load_quads, scope_term, subject_to_term, Batch, Probed, QuadSink, SinkTimer};
use crate::store::Store;
use crate::term::DEFAULT_GRAPH;
use oxrdf::{Term, Triple};
use oxttl::NTriplesParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub use crate::loader::LoadStats;

/// Load an N-Triples file. Reads the document into memory and parses it on
/// [`load_threads`] threads when the thread count and the file size both make
/// that worthwhile — see [`should_read_whole_file`]; otherwise streams it on
/// one thread with no full-file buffer.
///
/// The size ceiling matters: since HDB-96 threads are the default, so the
/// read-into-memory branch is the default too. Past
/// [`max_slice_bytes`](crate::loader::parallel::max_slice_bytes) this falls
/// back to streaming rather than allocating a copy of an arbitrarily large
/// document.
pub fn load_ntriples_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let mut stats = if should_read_whole_file(bytes, load_threads()) {
        drop(file);
        load_ntriples_slice(store, &std::fs::read(path)?)?
    } else {
        load_ntriples_reader(store, BufReader::with_capacity(1 << 20, file))?
    };
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_ntriples_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let parser = NTriplesParser::new();
    let tag = store.next_bnode_doc_tag();
    load_quads(
        store,
        parser.for_reader(reader).map(move |t| {
            let triple = t.map_err(|e| StorageError::NtriplesParse(format!("{e}")))?;
            Ok((
                DEFAULT_GRAPH,
                scope_term(tag, subject_to_term(triple.subject)),
                Term::NamedNode(triple.predicate),
                scope_term(tag, triple.object),
            ))
        }),
    )
}

/// Load an in-memory N-Triples document, parsing on [`load_threads`] threads.
///
/// Interning stays on the calling thread and runs in document order, so the
/// resulting store — triples, dictionary contents, and term ids — is identical
/// to what [`load_ntriples_reader`] produces for the same bytes.
pub fn load_ntriples_slice(store: &Store, bytes: &[u8]) -> Result<LoadStats> {
    load_ntriples_slice_with_threads(store, bytes, slice_threads(bytes.len()))
}

/// [`load_ntriples_slice`] with an explicit parse-thread count. `threads <= 1`
/// parses serially; anything higher splits regardless of document size.
pub fn load_ntriples_slice_with_threads(
    store: &Store,
    bytes: &[u8],
    threads: usize,
) -> Result<LoadStats> {
    let mut sink = QuadSink::new(store);
    let mut timer = SinkTimer::new();
    let tag = store.next_bnode_doc_tag();
    for_each_ntriples_probed(bytes, threads, tag, store.dictionary(), |batch| {
        timer.sink(|| {
            sink.intern_batch(|s| match batch {
                Batch::Raw(rows) => {
                    for t in rows {
                        s.push(
                            DEFAULT_GRAPH,
                            &scope_term(tag, subject_to_term(t.subject)),
                            &Term::NamedNode(t.predicate),
                            &scope_term(tag, t.object),
                        )?;
                    }
                    Ok(())
                }
                Batch::Probed(rows) => {
                    for row in rows {
                        s.push_probed(DEFAULT_GRAPH, row)?;
                    }
                    Ok(())
                }
            })
        })
    })?;
    timer.record_parse(sink.total);
    sink.finish()
}

/// [`for_each_ntriples_batch`], with each row's terms probed against `dict` on
/// the parse thread that produced it (HDB-106).
///
/// Skipped below [`crate::loader::parallel::MIN_PROBE_CHUNKS`] chunks
/// ([`should_probe`]): at one chunk there is no other thread to move the lookup
/// to, and at two or three the parse threads have no spare capacity to absorb
/// it — measurements on the constant.
pub(crate) fn for_each_ntriples_probed<F>(
    bytes: &[u8],
    threads: usize,
    tag: u64,
    dict: &Dictionary,
    sink: F,
) -> Result<()>
where
    F: FnMut(Batch<Triple, Probed>) -> Result<()>,
{
    let chunks = ntriples_chunks(bytes, threads);
    let probe = should_probe(chunks.len());
    parse_chunks_mapped(
        chunks,
        move |rows: Vec<Triple>| {
            if !probe {
                return Batch::Raw(rows);
            }
            Batch::Probed(
                rows.into_iter()
                    .map(|t| {
                        Probed::probe(
                            dict,
                            scope_term(tag, subject_to_term(t.subject)),
                            Term::NamedNode(t.predicate),
                            scope_term(tag, t.object),
                        )
                    })
                    .collect(),
            )
        },
        sink,
    )
}

/// Parse an in-memory N-Triples document on `threads` threads, handing `sink`
/// batches of triples **in document order**. `threads <= 1` parses serially;
/// no size floor is applied here (see [`load_ntriples_slice`] for that).
///
/// This is the shared primitive behind [`load_ntriples_slice`] and any caller
/// that wants parallel parsing without HornDB's store (the trainmarks driver
/// collects into its own vector, for one).
pub fn for_each_ntriples_batch<F>(bytes: &[u8], threads: usize, sink: F) -> Result<()>
where
    F: FnMut(Vec<Triple>) -> Result<()>,
{
    parse_chunks_ordered(ntriples_chunks(bytes, threads), sink)
}

/// One independently parseable chunk iterator per parse thread, or a single
/// serial one when `threads <= 1`.
fn ntriples_chunks(
    bytes: &[u8],
    threads: usize,
) -> Vec<Box<dyn Iterator<Item = Result<Triple>> + Send + '_>> {
    let parser = NTriplesParser::new();
    let parsers = if threads > 1 {
        parser.split_slice_for_parallel_parsing(bytes, threads)
    } else {
        vec![parser.for_slice(bytes)]
    };
    parsers
        .into_iter()
        .map(|p| {
            Box::new(p.map(|t| t.map_err(|e| StorageError::NtriplesParse(format!("{e}")))))
                as Box<dyn Iterator<Item = Result<Triple>> + Send + '_>
        })
        .collect()
}
