//! Concurrent term↔ID dictionary.
//!
//! Forward map: `DashMap<Term, TermId>` (lock-free reads, sharded writes).
//! Reverse map: `RwLock<Vec<Term>>` indexed by `payload - 1`.

use crate::error::{Result, StorageError};
use crate::term::{TermId, TermKind, MAX_DICT_INDEX};
use dashmap::DashMap;
use oxrdf::{Literal, NamedNodeRef, Term};
use parking_lot::RwLock;

const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

pub struct Dictionary {
    forward: DashMap<Term, TermId>,
    reverse: RwLock<Vec<Term>>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self {
            forward: DashMap::new(),
            reverse: RwLock::new(Vec::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.reverse.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn intern(&self, term: &Term) -> Result<TermId> {
        // Inline-int fast path.
        if let Some(id) = try_inline_int(term) {
            return Ok(id);
        }
        if let Some(existing) = self.forward.get(term) {
            return Ok(*existing);
        }
        // Slow path: acquire writer lock on reverse vec, double-check, append.
        let mut reverse = self.reverse.write();
        if let Some(existing) = self.forward.get(term) {
            return Ok(*existing);
        }
        let next_index = (reverse.len() as u64) + 1;
        if next_index >= MAX_DICT_INDEX {
            return Err(StorageError::DictionaryFull(next_index));
        }
        let kind = kind_of(term);
        let id = TermId::new(kind, next_index);
        reverse.push(term.clone());
        self.forward.insert(term.clone(), id);
        Ok(id)
    }

    pub fn lookup(&self, id: TermId) -> Option<Term> {
        if id.kind() == TermKind::InlineInt {
            let v = id.as_inline_int().unwrap();
            return Some(Term::Literal(Literal::new_typed_literal(
                v.to_string(),
                NamedNodeRef::new(XSD_INTEGER).unwrap(),
            )));
        }
        let idx = id.payload();
        if idx == 0 {
            return None;
        }
        let reverse = self.reverse.read();
        reverse.get((idx - 1) as usize).cloned()
    }
}

impl Default for Dictionary {
    fn default() -> Self {
        Self::new()
    }
}

fn kind_of(term: &Term) -> TermKind {
    match term {
        Term::NamedNode(_) => TermKind::Uri,
        Term::BlankNode(_) => TermKind::Blank,
        Term::Literal(lit) => {
            if lit.language().is_some() {
                TermKind::LangLiteral
            } else if lit.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#string" {
                TermKind::PlainLiteral
            } else {
                TermKind::TypedLiteral
            }
        }
        // RDF-star triples-as-terms (Term::Triple, gated behind oxrdf's "rdf-star"
        // feature) are out of Stage-1 scope. Feature unification across the
        // workspace can enable that variant transitively (oxigraph pulls it in),
        // so a catch-all keeps the match exhaustive in both feature configurations.
        // The N-Triples loader is the only path that produces a Term in Stage 1
        // and cannot emit RDF-star, so the arm is unreachable in practice.
        #[allow(unreachable_patterns)]
        _ => unreachable!("RDF-star quoted triples are not a Stage-1 input"),
    }
}

fn try_inline_int(term: &Term) -> Option<TermId> {
    if let Term::Literal(lit) = term {
        if lit.datatype().as_str() == XSD_INTEGER {
            if let Ok(v) = lit.value().parse::<i32>() {
                return Some(TermId::inline_int(v));
            }
        }
    }
    None
}
