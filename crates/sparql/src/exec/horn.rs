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
pub(crate) fn lexical_to_oxrdf(s: &str) -> OxTerm {
    if s.starts_with('"') {
        OxTerm::Literal(parse_literal(s))
    } else if let Some(label) = s.strip_prefix("_:") {
        OxTerm::BlankNode(BlankNode::new_unchecked(label))
    } else {
        OxTerm::NamedNode(NamedNode::new_unchecked(s))
    }
}

/// Statistics returned by [`load_with_reasoning`].
#[cfg(feature = "reasoner")]
#[derive(Debug, Clone, Copy)]
pub struct ReasonStats {
    /// Triples loaded into the backend (asserted base + inferred).
    pub loaded: u64,
    /// Asserted triples in the input dataset's default graph.
    pub asserted: usize,
}

/// Run the OWL 2 RL `horndb_owlrl` `Engine` (RuleFiring backend) over
/// `dataset`'s default graph and load the full materialized closure —
/// asserted base plus everything inferred — into `backend`.
#[cfg(feature = "reasoner")]
pub fn load_with_reasoning(
    backend: &mut HornBackend,
    dataset: &oxrdf::Dataset,
) -> Result<ReasonStats> {
    let mut engine = horndb_owlrl::integration::Engine::new();
    engine
        .load(dataset)
        .map_err(|e| SparqlError::Executor(format!("owlrl load: {e}")))?;
    let asserted = engine.asserted_len().unwrap_or(0);
    let triples = engine
        .materialized_triples()
        .ok_or_else(|| SparqlError::Executor("owlrl produced no state".into()))?;
    let loaded = backend.load_lexical_triples(triples.into_iter())?;
    Ok(ReasonStats { loaded, asserted })
}

use crate::algebra::TriplePattern;
use crate::exec::{Bindings, Executor, Store};
use arrow::array::UInt64Array;
use horndb_storage::{Store as ColumnStore, TermId};
use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::Executor as WcojExecutor;
use horndb_wcoj::ids::Triple as WTriple;
use horndb_wcoj::pattern::{Bgp as WBgp, Term as WTerm, TriplePattern as WPattern, Var as WVar};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use std::collections::{HashMap, HashSet};
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
    /// Mirror of every `(s, p, o)` TermId key ever physically written to
    /// `store` (updated after each successful `insert_triples`; never
    /// shrinks — storage is insertion-only). Enables O(1) membership
    /// tests without re-scanning the storage columns.
    stored_keys: HashSet<(u64, u64, u64)>,
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
            stored_keys: HashSet::new(),
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
        let existed = self.is_in_storage(key);
        let was_tombstoned = self.tombstones.remove(&key);
        if !existed {
            self.store
                .insert_triples(&[(s.clone(), p.clone(), o.clone())])
                .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
            self.stored_keys.insert(key);
        }
        let newly_live = !existed || was_tombstoned;
        if newly_live {
            self.live += 1;
            self.invalidate();
        }
        Ok(newly_live)
    }

    /// Bulk-insert oxrdf triples in one storage batch. Returns the number of
    /// newly-live triples. Same dedupe/tombstone semantics as `insert_oxrdf`;
    /// the columnar tier rebuilds each predicate partition at most once, and the
    /// snapshot is invalidated once at the end.
    ///
    /// Uses a read-compute / write-commit split to keep the storage insert
    /// correct even when intern errors occur:
    ///
    /// * Phase 1 (read-only): intern all terms, build the `to_store` batch and
    ///   record which keys are new or tombstone-resurrected. Any intern failure
    ///   skips that triple.
    /// * Phase 2 (write): call `store.insert_triples` once, then apply the
    ///   bookkeeping mutations (stored_keys, tombstones, live count) only on
    ///   success. Propagates storage errors.
    /// * Phase 3: invalidate the snapshot iff any triple became newly live
    ///   (covers the tombstone-resurrection-only case where `to_store` is
    ///   empty but the live set changed).
    pub fn insert_oxrdf_batch(
        &mut self,
        triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)>,
    ) -> Result<u64> {
        if triples.is_empty() {
            return Ok(0);
        }

        // Phase 1 (read-only): intern and classify each triple.
        // `intra_batch` deduplicates within the batch itself.
        struct Entry {
            key: (u64, u64, u64),
            ox: (oxrdf::Term, oxrdf::Term, oxrdf::Term),
            is_new_to_storage: bool,
            was_tombstoned: bool,
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(triples.len());
        let mut intra_batch: HashSet<(u64, u64, u64)> = HashSet::new();
        {
            let d = self.store.dictionary();
            for (s, p, o) in triples {
                let (si, pi, oi) = match (d.intern(&s), d.intern(&p), d.intern(&o)) {
                    (Ok(a), Ok(b), Ok(c)) => (a.0, b.0, c.0),
                    _ => continue, // intern failure — skip (consistent with insert_oxrdf)
                };
                let key = (si, pi, oi);
                if !intra_batch.insert(key) {
                    // Duplicate within this batch; first occurrence wins.
                    continue;
                }
                let is_new_to_storage = !self.is_in_storage(key);
                let was_tombstoned = self.tombstones.contains(&key);
                entries.push(Entry {
                    key,
                    ox: (s, p, o),
                    is_new_to_storage,
                    was_tombstoned,
                });
            }
        }

        // Collect triples that need to go to storage (never written before).
        let to_store: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> = entries
            .iter()
            .filter(|e| e.is_new_to_storage)
            .map(|e| e.ox.clone())
            .collect();

        // Phase 2 (write): storage insert first, then bookkeeping.
        if !to_store.is_empty() {
            self.store
                .insert_triples(&to_store)
                .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
        }

        // Bookkeeping (only reached if storage insert succeeded).
        let mut newly_live: u64 = 0;
        for e in &entries {
            if e.is_new_to_storage {
                self.stored_keys.insert(e.key);
            }
            if e.was_tombstoned {
                self.tombstones.remove(&e.key);
            }
            if e.is_new_to_storage || e.was_tombstoned {
                self.live += 1;
                newly_live += 1;
            }
        }

        // Phase 3: invalidate iff anything became live (covers resurrection).
        if newly_live > 0 {
            self.invalidate();
        }

        Ok(newly_live)
    }

    /// Bulk-insert algebra triples in one pass — O(n) cost versus O(n²) for
    /// repeated `insert_triple` calls when many triples share a predicate.
    ///
    /// Variables and RDF 1.2 triple terms are silently ignored (same as
    /// `Store::insert_triple`). Delegates to [`insert_oxrdf_batch`].
    pub fn insert_algebra_triples_bulk(&mut self, triples: Vec<(Term, Term, Term)>) {
        let ox_triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> = triples
            .into_iter()
            .filter_map(|(s, p, o)| {
                Some((
                    algebra_to_oxrdf(&s).ok()?,
                    algebra_to_oxrdf(&p).ok()?,
                    algebra_to_oxrdf(&o).ok()?,
                ))
            })
            .collect();
        // Ignore count; callers that need it should call insert_oxrdf_batch directly.
        let _ = self.insert_oxrdf_batch(ox_triples);
    }

    /// Bulk-load lexical triples in the `Engine::materialized_triples()`
    /// convention (IRIs bare, bnodes `_:`-prefixed, literals N-Triples).
    pub fn load_lexical_triples(
        &mut self,
        triples: impl Iterator<Item = (String, String, String)>,
    ) -> Result<u64> {
        let ox_triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> = triples
            .map(|(s, p, o)| {
                (
                    lexical_to_oxrdf(&s),
                    lexical_to_oxrdf(&p),
                    lexical_to_oxrdf(&o),
                )
            })
            .collect();
        self.insert_oxrdf_batch(ox_triples)
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

    /// True iff the triple is physically present in the insertion-only storage
    /// layer, whether live or tombstoned.
    fn is_in_storage(&self, key: (u64, u64, u64)) -> bool {
        self.stored_keys.contains(&key)
    }

    /// Get-or-build the WCOJ snapshot.
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
        if !self.tombstones.contains(&key) && self.is_in_storage(key) {
            self.tombstones.insert(key);
            self.live -= 1;
            self.invalidate();
        }
    }
}

impl Executor for HornBackend {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        // The empty BGP is the unit of join: exactly one empty solution
        // (parity with MemStore and the SPARQL algebra).
        if patterns.is_empty() {
            return Ok(Box::new(std::iter::once(Bindings::new())));
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        // SPARQL variable name -> WCOJ var index, first-appearance order.
        let mut var_index: HashMap<String, u8> = HashMap::new();
        // (original, alias) pairs introduced for variables repeated
        // *within a single pattern* — the trie executor must not see the
        // same WVar twice in one pattern, so the repeat becomes a fresh
        // alias plus a post-filter to the diagonal.
        let mut diagonal_filters: Vec<(String, String)> = Vec::new();
        let mut wpatterns: Vec<WPattern> = Vec::new();
        let mut ground: Vec<WTriple> = Vec::new();

        for pattern in patterns {
            let mut seen_here: HashSet<&str> = HashSet::new();
            let mut slots = [WTerm::Var(WVar(0)); 3];
            let mut all_bound = true;
            let slot_terms = [&pattern.subject, &pattern.predicate, &pattern.object];
            for (slot_no, term) in slot_terms.into_iter().enumerate() {
                slots[slot_no] = match term {
                    Term::Var(v) => {
                        all_bound = false;
                        let name = v.name();
                        let effective = if seen_here.contains(name) {
                            let alias = format!("__horndb_dup_{name}_{slot_no}");
                            diagonal_filters.push((name.to_owned(), alias.clone()));
                            alias
                        } else {
                            seen_here.insert(name);
                            name.to_owned()
                        };
                        let idx = match var_index.get(&effective) {
                            Some(&i) => i,
                            None => {
                                let next = var_index.len();
                                if next > u8::MAX as usize {
                                    return Err(SparqlError::Executor(
                                        "BGP exceeds 256 distinct variables".into(),
                                    ));
                                }
                                var_index.insert(effective, next as u8);
                                next as u8
                            }
                        };
                        WTerm::Var(WVar(idx))
                    }
                    constant => {
                        let ox = algebra_to_oxrdf(constant)?;
                        match dict.get(&ox) {
                            Some(id) => WTerm::Bound(id.0),
                            // A constant the dictionary has never seen
                            // cannot match any stored triple.
                            None => return Ok(Box::new(std::iter::empty())),
                        }
                    }
                };
            }
            if all_bound {
                let ids: Vec<u64> = slots.iter().map(|t| t.as_bound().unwrap()).collect();
                ground.push(WTriple::new(ids[0], ids[1], ids[2]));
            } else {
                wpatterns.push(WPattern::new(slots[0], slots[1], slots[2]));
            }
        }

        // Fully-ground patterns are membership tests against the snapshot;
        // any miss zeroes the whole BGP.
        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Box::new(std::iter::empty()));
        }
        // All patterns ground and present: one empty row (ASK semantics).
        if wpatterns.is_empty() {
            return Ok(Box::new(std::iter::once(Bindings::new())));
        }

        let bgp = WBgp::new(wpatterns);
        let mut rows: Vec<Bindings> = Vec::new();
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let schema = batch.schema();
            // Resolve each variable's column once per batch.
            let mut cols: Vec<(&str, &UInt64Array)> = Vec::with_capacity(var_index.len());
            for (name, idx) in &var_index {
                // Defensive: skip vars the executor produced no column for.
                let Some((col_idx, _)) = schema.column_with_name(&format!("v{idx}")) else {
                    continue;
                };
                let arr = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .ok_or_else(|| {
                        SparqlError::Executor(format!("wcoj batch column v{idx} is not UInt64"))
                    })?;
                cols.push((name.as_str(), arr));
            }
            for row in 0..batch.num_rows() {
                let mut b = Bindings::new();
                for &(name, arr) in &cols {
                    let id = TermId(arr.value(row));
                    let ox = dict
                        .lookup(id)
                        .ok_or_else(|| SparqlError::Executor(format!("dangling TermId {id:?}")))?;
                    b.set(name, oxrdf_to_algebra(&ox));
                }
                rows.push(b);
            }
        }

        // Diagonal filters: keep rows where each alias equals its
        // original, then strip the alias bindings from the output.
        if !diagonal_filters.is_empty() {
            rows.retain(|b| {
                diagonal_filters
                    .iter()
                    .all(|(orig, alias)| b.get(orig) == b.get(alias))
            });
            let aliases: HashSet<&str> = diagonal_filters.iter().map(|(_, a)| a.as_str()).collect();
            rows = rows
                .into_iter()
                .map(|b| {
                    let mut out = Bindings::new();
                    for (k, v) in b.vars() {
                        if !aliases.contains(k) {
                            out.set(k, v.clone());
                        }
                    }
                    out
                })
                .collect();
        }

        Ok(Box::new(rows.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;
    use horndb_wcoj::source::TripleSource;

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

    #[test]
    fn double_delete_does_not_underflow_live() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        );
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn bulk_resurrect_of_tombstoned_triple_refreshes_snapshot() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        );
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        let _ = b.wcoj_snapshot(); // warm: snapshot now has 0 triples
        b.insert_algebra_triples_bulk(vec![(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        )]);
        assert_eq!(b.len(), 1);
        assert_eq!(
            b.wcoj_snapshot().total_triples(),
            1,
            "snapshot must be rebuilt after a bulk resurrect"
        );
    }

    #[test]
    fn mutations_with_warm_snapshot_stay_consistent() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        );
        let _ = b.wcoj_snapshot(); // warm the cache
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        assert_eq!(b.len(), 0);
        let snap = b.wcoj_snapshot();
        assert_eq!(
            snap.total_triples(),
            0,
            "rebuilt snapshot must reflect the delete"
        );
    }
}
