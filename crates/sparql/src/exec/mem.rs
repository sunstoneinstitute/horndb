//! Hash-set backed in-memory triple store. Stage 1 only.
//!
//! Triples are stored as `(String, String, String)` — i.e. all terms
//! are kept as their N-Triples lexical form. This is intentionally
//! simple; SPEC-02 introduces the real dictionary-encoded store.

use crate::algebra::{Term, TriplePattern};
use crate::error::Result;
use crate::exec::{unify_one, Bindings, Executor, Store};
use std::collections::HashSet;

/// In-memory triple store. Clone-on-write semantics — each
/// `MemStore` is independent.
#[derive(Debug, Default, Clone)]
pub struct MemStore {
    triples: HashSet<(String, String, String)>,
}

impl MemStore {
    /// Insert a single triple from raw lexical-form strings.
    pub fn insert(&mut self, triple: (String, String, String)) {
        self.triples.insert(triple);
    }
    /// Number of triples currently stored. Stable; useful in tests.
    pub fn len(&self) -> usize {
        self.triples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }
}

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => panic!("term_to_lex called on Var({})", v.name()),
        // RDF 1.2 triple-term patterns are gated by SparqlConfig::rdf12
        // at translation time; the planner only sees them on the rdf12
        // path, which the Stage-1 MemStore does not implement.
        Term::Triple(_) => panic!(
            "term_to_lex called on Term::Triple (rdf-12 patterns are unsupported by MemStore)"
        ),
    }
}

impl Executor for MemStore {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        // Naive left-deep nested loop. Adequate for our test sizes
        // (W3C suite fixtures are tiny). SPEC-03 will replace this.
        let mut current: Vec<Bindings> = vec![Bindings::new()];
        for pat in patterns {
            let mut next: Vec<Bindings> = Vec::new();
            for row in &current {
                for triple in &self.triples {
                    if let Some(b) = unify_one(pat, triple, row) {
                        next.push(b);
                    }
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        Ok(Box::new(current.into_iter()))
    }
}

impl Store for MemStore {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        self.triples.insert((
            term_to_lex(&subject),
            term_to_lex(&predicate),
            term_to_lex(&object),
        ));
    }
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        self.triples.remove(&(
            term_to_lex(subject),
            term_to_lex(predicate),
            term_to_lex(object),
        ));
    }
}
