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

    /// Intern a subject/predicate/object triple in one call, returning their
    /// `TermId`s. Convenience over three [`Dictionary::intern`] calls, shared by
    /// the bulk loaders and [`crate::Store`]'s insert paths.
    pub fn intern_triple(&self, s: &Term, p: &Term, o: &Term) -> Result<(TermId, TermId, TermId)> {
        Ok((self.intern(s)?, self.intern(p)?, self.intern(o)?))
    }

    /// Resolve a term to its `TermId` **without** interning it. Returns
    /// `None` if the term has never been interned (inline-int literals
    /// always resolve — they are value-encoded, not dictionary-allocated).
    /// Used by query frontends to look up constants: an absent constant
    /// means no stored triple can match it.
    pub fn get(&self, term: &Term) -> Option<TermId> {
        if let Some(id) = try_inline_int(term) {
            return Some(id);
        }
        self.forward.get(term).map(|e| *e)
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
        // RDF 1.2 triple terms — see SPEC-00 (vision) and TASKS.md (PR2 of
        // the RDF 1.2 migration). `Term` implements `Hash + Eq` recursively,
        // so the forward `DashMap<Term, TermId>` deduplicates identical
        // triple terms automatically; the reverse `Vec<Term>` stores the
        // full `Term::Triple` recursively.
        Term::Triple(_) => TermKind::TripleTerm,
    }
}

fn try_inline_int(term: &Term) -> Option<TermId> {
    if let Term::Literal(lit) = term {
        if lit.datatype().as_str() == XSD_INTEGER {
            if let Ok(v) = lit.value().parse::<i32>() {
                // Inline only the canonical lexical form: non-canonical
                // variants ("042", "+42") must keep their own dictionary
                // identity, because RDF term equality is lexical and the
                // inline encoding can only round-trip the canonical form.
                if lit.value() == v.to_string() {
                    return Some(TermId::inline_int(v));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    #[test]
    fn get_returns_id_without_interning() {
        let d = Dictionary::new();
        let t = Term::NamedNode(NamedNode::new("http://ex/a").unwrap());
        assert_eq!(d.get(&t), None);
        assert_eq!(d.len(), 0, "get must not intern");
        let id = d.intern(&t).unwrap();
        assert_eq!(d.get(&t), Some(id));
    }

    #[test]
    fn get_resolves_inline_int_without_interning() {
        let d = Dictionary::new();
        let t = Term::Literal(Literal::new_typed_literal(
            "42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let id = d.get(&t).expect("inline ints always resolve");
        assert_eq!(id, TermId::inline_int(42));
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn non_canonical_integer_keeps_distinct_identity() {
        let d = Dictionary::new();
        let canon = Term::Literal(Literal::new_typed_literal(
            "42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let padded = Term::Literal(Literal::new_typed_literal(
            "042",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let plus = Term::Literal(Literal::new_typed_literal(
            "+42",
            NamedNodeRef::new(XSD_INTEGER).unwrap(),
        ));
        let id_canon = d.intern(&canon).unwrap();
        let id_padded = d.intern(&padded).unwrap();
        let id_plus = d.intern(&plus).unwrap();
        assert_eq!(id_canon, TermId::inline_int(42));
        assert_ne!(id_padded, id_canon);
        assert_ne!(id_plus, id_canon);
        assert_ne!(id_padded, id_plus);
        // Exact lexical round-trip for the non-canonical forms.
        assert_eq!(d.lookup(id_padded), Some(padded));
        assert_eq!(d.lookup(id_plus), Some(plus));
    }
}
