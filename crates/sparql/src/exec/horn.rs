//! `HornBackend` — the storage/WCOJ-backed implementation of the
//! [`Executor`](crate::exec::Executor) + [`Store`](crate::exec::Store)
//! seam (SPEC-07 wiring increment, issue #67).
//!
//! Term identity lives in `horndb_storage::Dictionary` (kind-tagged
//! `TermId`s — fixes the Stage-1 lexical type erasure). BGPs execute on
//! the SPEC-03 Leapfrog Triejoin over a lazily-rebuilt sorted snapshot.

use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::{literal_parts, unescape_ntriples};
use oxrdf::{BlankNode, Literal, NamedNode, Term as OxTerm};

/// algebra::Term constant -> oxrdf::Term (dictionary key form).
/// Errors on variables and RDF 1.2 triple terms.
///
/// # Literal normalization
///
/// oxrdf applies two normalizations that are consistent on both the data path
/// and the query path (both go through oxrdf), so matching stays correct even
/// though the lexical strings may not be byte-identical to the original input:
///
/// * **`xsd:string` collapsing** — `"v"^^<http://www.w3.org/2001/XMLSchema#string>`
///   round-trips as the plain form `"v"` (RDF 1.1 §3.3 says plain literals and
///   `xsd:string` literals are the same node).
/// * **BCP-47 language-tag lowercasing** — `"x"@EN` round-trips as `"x"@en`.
///
/// Callers that persist or compare the algebra `Term::Literal` form after a
/// round-trip should expect these normalizations rather than byte identity.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn algebra_to_oxrdf(t: &Term) -> Result<OxTerm> {
    match t {
        Term::Iri(s) => Ok(OxTerm::NamedNode(NamedNode::new_unchecked(s.clone()))),
        Term::BlankNode(s) => Ok(OxTerm::BlankNode(BlankNode::new_unchecked(s.clone()))),
        Term::Literal(raw) => Ok(OxTerm::Literal(parse_literal(raw))),
        Term::Var(v) => Err(SparqlError::Executor(format!(
            "algebra_to_oxrdf called on variable ?{}",
            v.name()
        ))),
        Term::Triple(_) => Err(SparqlError::Executor(
            "RDF 1.2 triple terms are not supported by the storage backend yet".into(),
        )),
    }
}

/// N-Triples literal lexical form -> oxrdf::Literal.
/// `literal_parts` keeps the value escaped; unescape before building.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
fn parse_literal(raw: &str) -> Literal {
    let (escaped, lang, dt) = literal_parts(raw);
    let value = unescape_ntriples(&escaped);
    match (lang, dt) {
        (Some(lang), _) => Literal::new_language_tagged_literal(&value, lang)
            .unwrap_or_else(|_| Literal::new_simple_literal(value)),
        (None, Some(dt)) => Literal::new_typed_literal(value, NamedNode::new_unchecked(dt)),
        (None, None) => Literal::new_simple_literal(value),
    }
}

/// oxrdf::Term -> algebra::Term, preserving kind (the point of #67).
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn oxrdf_to_algebra(t: &OxTerm) -> Term {
    match t {
        OxTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        OxTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        OxTerm::Literal(l) => Term::Literal(l.to_string()),
        // Triple terms never enter the backend (rejected on insert/lookup),
        // so this arm is unreachable in practice; degrade gracefully.
        #[allow(unreachable_patterns)]
        other => Term::Iri(other.to_string()),
    }
}

/// One lexical term in the `Engine::materialized_triples()` convention:
/// leading `"` = literal (N-Triples form), leading `_:` = blank node
/// (prefix stripped), anything else = bare IRI.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn lexical_to_oxrdf(s: &str) -> OxTerm {
    if s.starts_with('"') {
        OxTerm::Literal(parse_literal(s))
    } else if let Some(label) = s.strip_prefix("_:") {
        OxTerm::BlankNode(BlankNode::new_unchecked(label))
    } else {
        OxTerm::NamedNode(NamedNode::new_unchecked(s))
    }
}

use crate::exec::Store;
use horndb_storage::Store as ColumnStore;
use horndb_wcoj::ids::Triple as WTriple;
use horndb_wcoj::source::vec_source::VecTripleSource;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Storage + WCOJ backed SPARQL backend (issue #67).
///
/// * Term identity: `horndb_storage::Dictionary` (kind-tagged TermIds).
/// * Reads: Leapfrog Triejoin over a lazily-built [`VecTripleSource`]
///   snapshot (all six orderings, rebuilt after any mutation — a
///   documented Stage-1 cost; see INTEGRATION-NOTES.md).
/// * Writes: storage is insertion-only at Stage 1, so `DELETE DATA`
///   maintains a tombstone overlay applied at snapshot-build time.
///
/// Two literals with the same *value* but different lexical forms of
/// `xsd:integer` (e.g. `"042"` vs `"42"`) share an inline-int TermId;
/// matching is value-based for small integers and bound values decode
/// to the canonical lexical form. This is closer to SPARQL value
/// semantics than the Stage-1 MemStore's pure lexical matching.
pub struct HornBackend {
    store: ColumnStore,
    /// Raw `(s, p, o)` TermId payloads currently deleted.
    tombstones: HashSet<(u64, u64, u64)>,
    /// Live triple count (storage's count minus active tombstones).
    live: u64,
    /// Lazily-built WCOJ source. `None` after any mutation.
    snapshot: Mutex<Option<Arc<VecTripleSource>>>,
}

impl Default for HornBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HornBackend {
    pub fn new() -> Self {
        Self {
            store: ColumnStore::in_memory(),
            tombstones: HashSet::new(),
            live: 0,
            snapshot: Mutex::new(None),
        }
    }

    /// Live triple count.
    pub fn len(&self) -> u64 {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn invalidate(&mut self) {
        *self.snapshot.get_mut().expect("snapshot lock poisoned") = None;
    }

    /// Insert one oxrdf triple. Returns true if it was new (i.e. live count increased).
    pub fn insert_oxrdf(
        &mut self,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        let key = self.intern_key(s, p, o)?;
        let existed = self.contains_key(key);
        let was_tombstoned = self.tombstones.remove(&key);
        if !existed {
            self.store
                .insert_triples(&[(s.clone(), p.clone(), o.clone())])
                .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
        }
        let newly_live = !existed || was_tombstoned;
        if newly_live {
            self.live += 1;
            self.invalidate();
        }
        Ok(newly_live)
    }

    /// Bulk-load lexical triples in the `Engine::materialized_triples()`
    /// convention (IRIs bare, bnodes `_:`-prefixed, literals N-Triples).
    pub fn load_lexical_triples(
        &mut self,
        triples: impl Iterator<Item = (String, String, String)>,
    ) -> Result<u64> {
        let mut n = 0;
        for (s, p, o) in triples {
            if self.insert_oxrdf(
                &lexical_to_oxrdf(&s),
                &lexical_to_oxrdf(&p),
                &lexical_to_oxrdf(&o),
            )? {
                n += 1;
            }
        }
        Ok(n)
    }

    fn intern_key(
        &self,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<(u64, u64, u64)> {
        let d = self.store.dictionary();
        let err = |e: horndb_storage::StorageError| SparqlError::Executor(format!("intern: {e}"));
        Ok((
            d.intern(s).map_err(err)?.0,
            d.intern(p).map_err(err)?.0,
            d.intern(o).map_err(err)?.0,
        ))
    }

    /// Membership against the *storage* layer (ignores tombstones).
    fn contains_key(&self, key: (u64, u64, u64)) -> bool {
        // Cheap path: consult the snapshot if it is current; otherwise
        // scan the (small) predicate partition.
        if let Some(snap) = self
            .snapshot
            .lock()
            .expect("snapshot lock poisoned")
            .as_ref()
        {
            return snap.contains(&WTriple::new(key.0, key.1, key.2))
                || self.tombstones.contains(&key);
        }
        self.store
            .scan_all_term_ids()
            .iter()
            .any(|t| (t.0 .0, t.1 .0, t.2 .0) == key)
    }

    /// Get-or-build the WCOJ snapshot.
    #[allow(dead_code)] // used by the Executor impl (Task 6)
    pub(crate) fn wcoj_snapshot(&self) -> Arc<VecTripleSource> {
        let mut guard = self.snapshot.lock().expect("snapshot lock poisoned");
        if let Some(s) = guard.as_ref() {
            return Arc::clone(s);
        }
        let triples: Vec<WTriple> = self
            .store
            .scan_all_term_ids()
            .into_iter()
            .map(|(s, p, o)| (s.0, p.0, o.0))
            .filter(|k| !self.tombstones.contains(k))
            .map(|(s, p, o)| WTriple::new(s, p, o))
            .collect();
        let built = Arc::new(VecTripleSource::from_triples(triples));
        *guard = Some(Arc::clone(&built));
        built
    }
}

impl Store for HornBackend {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        let (Ok(s), Ok(p), Ok(o)) = (
            algebra_to_oxrdf(&subject),
            algebra_to_oxrdf(&predicate),
            algebra_to_oxrdf(&object),
        ) else {
            // Variables / triple terms cannot reach INSERT DATA (the
            // parser only produces ground quads); ignore defensively.
            return;
        };
        let _ = self.insert_oxrdf(&s, &p, &o);
    }

    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        let (Ok(s), Ok(p), Ok(o)) = (
            algebra_to_oxrdf(subject),
            algebra_to_oxrdf(predicate),
            algebra_to_oxrdf(object),
        ) else {
            return;
        };
        let d = self.store.dictionary();
        // Non-interning lookups: a term the dictionary has never seen
        // cannot participate in any stored triple.
        let (Some(s), Some(p), Some(o)) = (d.get(&s), d.get(&p), d.get(&o)) else {
            return;
        };
        let key = (s.0, p.0, o.0);
        if self.contains_key(key) && self.tombstones.insert(key) {
            self.live -= 1;
            self.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;

    #[test]
    fn insert_and_delete_round_trip() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 1);
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 0);
        // Deleting an unknown triple is a no-op, not a panic.
        b.delete_triple(
            &Term::Iri("http://ex/nope".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        // Re-insert after delete resurrects the triple (tombstone cleared).
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Literal("\"v\"".into()),
        );
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn load_lexical_triples_accepts_owlrl_dump() {
        let mut b = HornBackend::new();
        b.load_lexical_triples(
            [
                (
                    "http://ex/s".to_owned(),
                    "http://ex/p".to_owned(),
                    "_:b0".to_owned(),
                ),
                (
                    "http://ex/s".to_owned(),
                    "http://ex/q".to_owned(),
                    "\"10\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_owned(),
                ),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn literal_round_trips_through_oxrdf() {
        for raw in [
            "\"hello\"",
            "\"hej\"@sv",
            "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "\"a \\\"quoted\\\" word\"",
        ] {
            let ox = algebra_to_oxrdf(&Term::Literal(raw.to_owned())).unwrap();
            // xsd:string normalisation: oxrdf may render plain literals
            // identically; the invariant is algebra->oxrdf->algebra fixpoint.
            let back = oxrdf_to_algebra(&ox);
            assert_eq!(back, Term::Literal(raw.to_owned()), "round trip of {raw}");
        }
    }

    #[test]
    fn iri_and_bnode_conventions_match_translate() {
        let iri = algebra_to_oxrdf(&Term::Iri("http://ex/a".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&iri), Term::Iri("http://ex/a".into()));
        let b = algebra_to_oxrdf(&Term::BlankNode("b0".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&b), Term::BlankNode("b0".into()));
    }

    #[test]
    fn lexical_convention_covers_owlrl_dump_forms() {
        assert!(matches!(
            lexical_to_oxrdf("http://ex/a"),
            OxTerm::NamedNode(_)
        ));
        match lexical_to_oxrdf("_:b0") {
            OxTerm::BlankNode(b) => assert_eq!(b.as_str(), "b0"),
            other => panic!("expected bnode, got {other:?}"),
        }
        assert!(matches!(lexical_to_oxrdf("\"x\"@en"), OxTerm::Literal(_)));
    }

    #[test]
    fn variables_are_rejected() {
        assert!(algebra_to_oxrdf(&Term::Var(Var::new("x"))).is_err());
    }

    #[test]
    fn explicit_xsd_string_normalizes_to_plain_form() {
        let raw = "\"v\"^^<http://www.w3.org/2001/XMLSchema#string>";
        let ox = algebra_to_oxrdf(&Term::Literal(raw.to_owned())).unwrap();
        assert_eq!(oxrdf_to_algebra(&ox), Term::Literal("\"v\"".to_owned()));
    }
}
