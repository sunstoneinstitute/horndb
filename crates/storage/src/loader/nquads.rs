//! N-Quads bulk loader — streaming and parallel-chunked.
//!
//! Routes each quad to the graph named by its fourth term (SPEC-02 F7) and
//! batches into the dictionary + tier in chunks of [`crate::loader::load_batch_triples`].
//! A quad with no graph term lands in the default graph (the reserved
//! [`DEFAULT_GRAPH`] sentinel); a named (IRI or blank-node) graph term is
//! interned and used as the graph id, so triples with the same graph label
//! co-locate.
//!
//! [`load_nquads_slice`] parses an in-memory document on [`load_threads`]
//! threads. As with N-Triples the split is line-based and unconditionally
//! safe, and graph interning stays on the calling thread in document order, so
//! graph ids match the streaming path exactly.

use crate::dictionary::Dictionary;
use crate::error::{Result, StorageError};
use crate::loader::parallel::{
    load_threads, parse_chunks_mapped, parse_chunks_ordered, should_read_whole_file, slice_threads,
};
use crate::loader::{
    load_quads_in_graphs, subject_to_term, LoadStats, Probed, QuadSink, SinkTimer,
};
use crate::store::Store;
use crate::term::{GraphId, DEFAULT_GRAPH};
use oxrdf::{GraphName, Quad, Term};
use oxttl::NQuadsParser;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Load an N-Quads file, parallel-chunked when the thread count and file size
/// make it worthwhile (see [`crate::loader::ntriples::load_ntriples_file`]).
pub fn load_nquads_file(store: &Store, path: &Path) -> Result<LoadStats> {
    let file = File::open(path)?;
    let bytes = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let mut stats = if should_read_whole_file(bytes, load_threads()) {
        drop(file);
        load_nquads_slice(store, &std::fs::read(path)?)?
    } else {
        load_nquads_reader(store, BufReader::with_capacity(1 << 20, file))?
    };
    stats.bytes_read = bytes;
    Ok(stats)
}

pub fn load_nquads_reader<R: Read>(store: &Store, reader: R) -> Result<LoadStats> {
    let parser = NQuadsParser::new();
    load_quads_in_graphs(
        store,
        parser.for_reader(reader).map(|q| {
            let quad = q.map_err(|e| StorageError::NquadsParse(format!("{e}")))?;
            Ok((
                quad.graph_name,
                subject_to_term(quad.subject),
                Term::NamedNode(quad.predicate),
                quad.object,
            ))
        }),
        graph_id,
    )
}

/// Load an in-memory N-Quads document, parsing on [`load_threads`] threads.
/// Produces the same store contents as [`load_nquads_reader`] on the same
/// bytes.
pub fn load_nquads_slice(store: &Store, bytes: &[u8]) -> Result<LoadStats> {
    load_nquads_slice_with_threads(store, bytes, slice_threads(bytes.len()))
}

/// [`load_nquads_slice`] with an explicit parse-thread count. `threads <= 1`
/// parses serially; anything higher splits regardless of document size.
pub fn load_nquads_slice_with_threads(
    store: &Store,
    bytes: &[u8],
    threads: usize,
) -> Result<LoadStats> {
    let mut sink = QuadSink::new(store);
    let mut timer = SinkTimer::new();
    for_each_nquads_probed(bytes, threads, store.dictionary(), |rows| {
        timer.sink(|| {
            sink.intern_batch(|s| {
                for (graph_name, row) in rows {
                    // The graph label is interned here, on the calling thread,
                    // before the row's own terms — the order the unprobed path
                    // used, so graph labels keep the ids they had.
                    let g = graph_id(store, graph_name)?;
                    s.push_probed(g, row)?;
                }
                Ok(())
            })
        })
    })?;
    timer.record_parse(sink.total);
    sink.finish()
}

/// [`for_each_nquads_batch`], with each row's subject/predicate/object probed
/// against `dict` on the parse thread that produced it (HDB-106).
///
/// The graph label is *not* probed: it is interned through
/// [`Store::intern_graph_uri`](crate::Store::intern_graph_uri), which allocates
/// a `GraphId` as well as a term, so it stays on the calling thread and is
/// carried alongside the probed row.
pub(crate) fn for_each_nquads_probed<F>(
    bytes: &[u8],
    threads: usize,
    dict: &Dictionary,
    sink: F,
) -> Result<()>
where
    F: FnMut(Vec<(GraphName, Probed)>) -> Result<()>,
{
    let chunks = nquads_chunks(bytes, threads);
    let probe = chunks.len() > 1;
    parse_chunks_mapped(
        chunks,
        move |q: Quad| {
            let s = subject_to_term(q.subject);
            let p = Term::NamedNode(q.predicate);
            let row = if probe {
                Probed::probe(dict, s, p, q.object)
            } else {
                Probed::unprobed(s, p, q.object)
            };
            (q.graph_name, row)
        },
        sink,
    )
}

/// Parse an in-memory N-Quads document on `threads` threads, handing `sink`
/// batches of quads in document order. See
/// [`crate::loader::ntriples::for_each_ntriples_batch`].
pub fn for_each_nquads_batch<F>(bytes: &[u8], threads: usize, sink: F) -> Result<()>
where
    F: FnMut(Vec<Quad>) -> Result<()>,
{
    parse_chunks_ordered(nquads_chunks(bytes, threads), sink)
}

/// One independently parseable chunk iterator per parse thread, or a single
/// serial one when `threads <= 1`.
fn nquads_chunks(
    bytes: &[u8],
    threads: usize,
) -> Vec<Box<dyn Iterator<Item = Result<Quad>> + Send + '_>> {
    let parser = NQuadsParser::new();
    let parsers = if threads > 1 {
        parser.split_slice_for_parallel_parsing(bytes, threads)
    } else {
        vec![parser.for_slice(bytes)]
    };
    parsers
        .into_iter()
        .map(|p| {
            Box::new(p.map(|q| q.map_err(|e| StorageError::NquadsParse(format!("{e}")))))
                as Box<dyn Iterator<Item = Result<Quad>> + Send + '_>
        })
        .collect()
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
