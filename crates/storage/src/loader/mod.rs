//! Bulk loaders.
//!
//! Stage-1 streaming loaders for N-Triples, Turtle, and N-Quads (SPEC-02 F8),
//! all built on `oxttl` streaming parsers feeding the dictionary + tier in
//! batches of [`BATCH_SIZE`]. N-Quads routes each quad to the graph named by
//! its fourth term (SPEC-02 F7); N-Triples and Turtle load the default graph.
pub mod nquads;
pub mod ntriples;
pub mod turtle;

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
    let start = Instant::now();
    let mut batch: Vec<(GraphId, TermId, TermId, TermId)> = Vec::with_capacity(BATCH_SIZE);
    let mut total: u64 = 0;

    for quad in quads {
        let (g, s, p, o) = quad?;
        let (s_id, p_id, o_id) = store.dictionary().intern_triple(&s, &p, &o)?;
        batch.push((g, s_id, p_id, o_id));
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
        bytes_read: 0, // file-level caller overwrites this
        elapsed_ms: start.elapsed().as_millis() as u64,
        dictionary_size: store.dictionary().len() as u64,
    })
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
