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

    /// Bulk-decode a batch of **inline-int** `TermId`s to `xsd:integer`
    /// literals. Non-inline ids decode to `None`. The i32 payloads are
    /// extracted by [`Dictionary::decode_inline_ints`] (the SIMD-friendly
    /// data-parallel core), then materialised to `Term::Literal`. SPEC-12 F2
    /// / acceptance #4.
    ///
    /// The vectorisable win is in the *integer extraction* (mask the kind tag,
    /// cast the low 32 payload bits across a batch); building the
    /// `Term::Literal` strings is inherently scalar (heap allocation) and
    /// dominates only when the caller needs full `Term`s. Callers that only
    /// need the i32 values should use [`Dictionary::decode_inline_ints`], which
    /// is the path the benchmark measures.
    pub fn lookup_inline_int_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let ints = Self::decode_inline_ints(ids);
        ints.into_iter()
            .map(|opt| {
                opt.map(|v| {
                    Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    ))
                })
            })
            .collect()
    }

    /// Extract the i32 value of each inline-int `TermId` in `ids`; `None` for
    /// any id that is not `TermKind::InlineInt`. This is the data-parallel hot
    /// core (mask the kind tag, cast the low 32 payload bits) — the form the
    /// decode microbench measures for the ≥4× floor (SPEC-12 NF4).
    ///
    /// Per SPEC-12 "measure first": the loop body is a pure mask+cast unpack
    /// that the compiler autovectorises; a dedicated `horndb-simd` unpack
    /// primitive is only added if the hornbench bench shows this misses ≥4×.
    pub fn decode_inline_ints(ids: &[TermId]) -> Vec<Option<i32>> {
        // The kind tag occupies bits [60,64); inline-int tag value:
        let inline_tag = (TermKind::InlineInt as u64) << crate::term::KIND_SHIFT;
        let tag_mask = !crate::term::PAYLOAD_MASK; // top 4 bits
        ids.iter()
            .map(|&id| {
                let bits = id.bits();
                if bits & tag_mask == inline_tag {
                    Some((bits as u32) as i32)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Bulk lookup over a **mixed** batch: inline ints decode arithmetically,
    /// everything else via the reverse map under a single read lock.
    pub fn lookup_batch(&self, ids: &[TermId]) -> Vec<Option<Term>> {
        let reverse = self.reverse.read();
        ids.iter()
            .map(|&id| {
                if id.kind() == TermKind::InlineInt {
                    let v = id.as_inline_int().unwrap();
                    Some(Term::Literal(Literal::new_typed_literal(
                        v.to_string(),
                        NamedNodeRef::new(XSD_INTEGER).unwrap(),
                    )))
                } else {
                    let idx = id.payload();
                    if idx == 0 {
                        None
                    } else {
                        reverse.get((idx - 1) as usize).cloned()
                    }
                }
            })
            .collect()
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
    fn lookup_inline_int_batch_matches_scalar() {
        let dict = Dictionary::new();
        let ids: Vec<TermId> = (-5..20).map(TermId::inline_int).collect();
        let want: Vec<Term> = ids.iter().map(|&id| dict.lookup(id).unwrap()).collect();
        let got = dict.lookup_inline_int_batch(&ids);
        assert_eq!(got.len(), ids.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.as_ref().unwrap(), w);
        }
    }

    #[test]
    fn lookup_batch_handles_mixed() {
        let dict = Dictionary::new();
        let iri = Term::NamedNode(oxrdf::NamedNode::new("http://example.org/a").unwrap());
        let iri_id = dict.intern(&iri).unwrap();
        let int_id = TermId::inline_int(42);
        let got = dict.lookup_batch(&[int_id, iri_id]);
        assert_eq!(got[0].as_ref().unwrap(), &dict.lookup(int_id).unwrap());
        assert_eq!(got[1].as_ref().unwrap(), &iri);
    }

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
