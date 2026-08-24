//! Bulk loaders.
//!
//! Stage-1 streaming loaders for N-Triples, Turtle, and N-Quads (SPEC-02 F8),
//! all built on `oxttl` streaming parsers feeding the dictionary + tier in
//! batches of [`BATCH_SIZE`]. N-Quads routes each quad to the graph named by
//! its fourth term (SPEC-02 F7); N-Triples and Turtle load the default graph.
pub mod nquads;
pub mod ntriples;
pub mod parallel;
pub mod turtle;

pub use parallel::{
    load_buffer_triples, load_threads, set_load_buffer_triples, DEFAULT_LOAD_BUFFER_TRIPLES,
};

use crate::error::Result;
use crate::store::Store;
use crate::term::{GraphId, TermId};
use oxrdf::{NamedOrBlankNode, Term};
use std::time::Instant;

/// Batch size for dictionary interning + tier insertion across all loaders.
pub(crate) const BATCH_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct LoadStats {
    pub triples: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub dictionary_size: u64,
}

/// Drive a stream of parsed quads into the store: intern each term, batch into
/// chunks of [`BATCH_SIZE`], and flush to the tier. Shared by every loader; each
/// format only differs in how it turns a parser item into a
/// `(graph, subject, predicate, object)` tuple (`bytes_read` is filled in by the
/// file-level entry points). The default graph uses
/// [`crate::term::DEFAULT_GRAPH`].
pub(crate) fn load_quads<I>(store: &Store, quads: I) -> Result<LoadStats>
where
    I: Iterator<Item = Result<(GraphId, Term, Term, Term)>>,
{
    let mut sink = QuadSink::new(store);
    for quad in quads {
        let (g, s, p, o) = quad?;
        sink.push(g, &s, &p, &o)?;
    }
    sink.finish()
}

/// Intern + batch + flush, shared by the streaming (`load_quads`) and the
/// slice/parallel loaders. Both drive it from a single thread in document
/// order, so a document loaded either way gets the same term ids.
pub(crate) struct QuadSink<'a> {
    store: &'a Store,
    batch: Vec<(GraphId, TermId, TermId, TermId)>,
    total: u64,
    start: Instant,
}

impl<'a> QuadSink<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self {
            store,
            batch: Vec::with_capacity(BATCH_SIZE),
            total: 0,
            start: Instant::now(),
        }
    }

    pub(crate) fn push(&mut self, g: GraphId, s: &Term, p: &Term, o: &Term) -> Result<()> {
        let (s_id, p_id, o_id) = self.store.dictionary().intern_triple(s, p, o)?;
        self.batch.push((g, s_id, p_id, o_id));
        self.total += 1;
        if self.batch.len() >= BATCH_SIZE {
            self.store.tier().insert_quad_batch(&self.batch)?;
            self.batch.clear();
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<LoadStats> {
        if !self.batch.is_empty() {
            self.store.tier().insert_quad_batch(&self.batch)?;
            self.batch.clear();
        }
        Ok(LoadStats {
            triples: self.total,
            bytes_read: 0, // file-level caller overwrites this
            elapsed_ms: self.start.elapsed().as_millis() as u64,
            dictionary_size: self.store.dictionary().len() as u64,
        })
    }
}

/// RDF 1.2's data model (oxrdf 0.3 with `rdf-12`) keeps subjects as the
/// 1.1-shaped `NamedOrBlankNode`: triple terms appear only in the object
/// position (oxrdf's `Term::Triple`). The match is exhaustive.
pub(crate) fn subject_to_term(s: NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b),
    }
}
