//! Bulk loaders.
//!
//! Stage-1 streaming loaders for N-Triples, Turtle, and N-Quads (SPEC-02 F8),
//! all built on `oxttl` streaming parsers feeding the dictionary + tier in
//! batches of [`BATCH_SIZE`]. N-Quads routes each quad to the graph named by
//! its fourth term (SPEC-02 F7); N-Triples and Turtle load the default graph.
pub mod nquads;
pub mod ntriples;
pub mod turtle;

use oxrdf::{NamedOrBlankNode, Term};

/// Batch size for dictionary interning + tier insertion across all loaders.
pub(crate) const BATCH_SIZE: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct LoadStats {
    pub triples: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub dictionary_size: u64,
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
