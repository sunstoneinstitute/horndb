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

/// Split an engine-key literal (`"<raw>"@lang` or `"<raw>"^^<dt>` with the
/// value RAW, not N-Triples-escaped) into its parts. The suffix is found by
/// scanning from the END: a datatype suffix is the last `"^^<` (datatype
/// IRIs cannot contain `"`), a language suffix is a trailing `@[A-Za-z0-9-]+`
/// immediately preceded by `"`. Embedded quotes in the raw value therefore
/// never mis-split.
fn engine_key_literal(key: &str) -> Literal {
    // Typed form: `"<raw>"^^<dt>`. Split at the LAST `"^^<` — the datatype
    // IRI cannot contain `"`, so anything before it belongs to the value.
    if key.ends_with('>') {
        if let Some(split) = key.rfind("\"^^<") {
            if split >= 1 {
                let value = &key[1..split]; // raw — no unescaping
                let dt = &key[split + 4..key.len() - 1];
                // oxrdf normalizes xsd:string typed literals to plain — fine.
                return Literal::new_typed_literal(value, NamedNode::new_unchecked(dt));
            }
        }
    }
    // Language form: `"<raw>"@lang` with lang = [A-Za-z0-9-]+ and the char
    // before the `@` being the closing quote.
    if let Some(at) = key.rfind('@') {
        let lang = &key[at + 1..];
        if !lang.is_empty()
            && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && at >= 2
            && key.as_bytes()[at - 1] == b'"'
        {
            let value = &key[1..at - 1]; // raw — no unescaping
            return Literal::new_language_tagged_literal(value, lang)
                .unwrap_or_else(|_| Literal::new_simple_literal(value));
        }
    }
    // Plain form: `"<raw>"`.
    if key.len() >= 2 && key.ends_with('"') {
        Literal::new_simple_literal(&key[1..key.len() - 1])
    } else {
        // Malformed key (no trailing quote) — degrade, don't panic.
        Literal::new_simple_literal(&key[1..])
    }
}

/// One lexical term in the `Engine::materialized_triples()` convention:
/// leading `"` = literal in engine-key form (`"<raw>"@lang` /
/// `"<raw>"^^<dt>` with the value RAW, **not** N-Triples-escaped), leading
/// `_:` = blank node (prefix stripped), anything else = bare IRI.
pub(crate) fn lexical_to_oxrdf(s: &str) -> OxTerm {
    if s.starts_with('"') {
        OxTerm::Literal(engine_key_literal(s))
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

use crate::algebra::{TriplePattern, Var};
use crate::exec::{Bindings, Executor, GroupCount, Slot, Store};
use arrow::array::UInt64Array;
use horndb_storage::{Store as ColumnStore, TermId};
use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::estimator::StatsEstimator;
use horndb_wcoj::executor::Executor as WcojExecutor;
use horndb_wcoj::ids::Triple as WTriple;
use horndb_wcoj::pattern::{Bgp as WBgp, Term as WTerm, TriplePattern as WPattern, Var as WVar};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::source::TripleSource;
use horndb_wcoj::stats::SnapshotStats;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Cheap size stats for scrape-time metrics (see [`HornBackend::storage_stats`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct HornStorageStats {
    pub triples: u64,
    pub graphs: u64,
    pub predicates: u64,
    pub dictionary_terms: u64,
    pub bytes_estimated: u64,
}

/// Storage + WCOJ backed SPARQL backend (issue #67).
///
/// * Term identity: `horndb_storage::Dictionary` (kind-tagged TermIds).
/// * Reads: Leapfrog Triejoin over a lazily-built [`VecTripleSource`]
///   snapshot (all six orderings, rebuilt after any mutation — a
///   documented Stage-1 cost; see INTEGRATION-NOTES.md).
/// * Writes: storage is insertion-only at Stage 1, so `DELETE DATA`
///   maintains a tombstone overlay applied at snapshot-build time.
///
/// RDF term identity is preserved: canonical-form `xsd:integer`
/// literals (e.g. `"42"`) use the dictionary's inline-int fast path,
/// while non-canonical lexical forms (`"042"`, `"+42"`) keep distinct
/// dictionary identities and round-trip their exact lexical form.
/// Matching is therefore term-based (lexical form + datatype), as
/// SPARQL BGP semantics require.
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
    /// Cached statistics summary derived from a specific snapshot, used by
    /// `EXPLAIN`'s `cardinality_estimate`. Holds the `Arc<VecTripleSource>` the
    /// stats were built from alongside the stats themselves. The cache
    /// self-invalidates: any write rebuilds the snapshot into a fresh `Arc`
    /// (see `invalidate` + `wcoj_snapshot`), so a stale entry never passes the
    /// `Arc::ptr_eq` identity check against the current snapshot.
    stats_cache: Mutex<Option<(Arc<VecTripleSource>, Arc<SnapshotStats>)>>,
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
            stats_cache: Mutex::new(None),
        }
    }

    /// Live triple count.
    pub fn len(&self) -> u64 {
        self.live
    }

    /// Cheap point-in-time size stats for scrape-time metrics: live triple
    /// count plus the tier's already-tracked graph/predicate/byte estimates and
    /// the dictionary term count. Bounded by the number of distinct
    /// predicates/graphs — never an O(triples) traversal. `triples` reflects
    /// the live count (storage is insertion-only, so the tier's own triple
    /// count would include tombstoned rows).
    pub fn storage_stats(&self) -> HornStorageStats {
        let tier = self.store.stats();
        HornStorageStats {
            triples: self.live,
            graphs: tier.graphs,
            predicates: tier.predicates,
            dictionary_terms: self.store.dictionary().len() as u64,
            bytes_estimated: tier.bytes_estimated,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn invalidate(&mut self) {
        *self.snapshot.get_mut().expect("snapshot lock poisoned") = None;
        // Clear the stats cache too: releases the obsolete snapshot's Arc (all six
        // sorted indexes) immediately rather than pinning it until the next estimate.
        *self
            .stats_cache
            .get_mut()
            .expect("stats_cache lock poisoned") = None;
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
                    _ => continue, // intern failure — skip this triple (lenient for bulk loads; the single-triple insert_oxrdf propagates instead)
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
    /// convention (IRIs bare, bnodes `_:`-prefixed, literals in engine-key
    /// form — quoted RAW value, not N-Triples-escaped).
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

    /// Materialize every live triple as oxrdf terms, for export /
    /// serialization (the load path is insertion-only and the server never
    /// dumps, so this is the read-back seam). Tombstoned triples are skipped;
    /// any triple whose TermIds no longer resolve in the dictionary is
    /// silently dropped (cannot happen for an insertion-only dictionary).
    pub fn iter_oxrdf(&self) -> Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> {
        let dict = self.store.dictionary();
        self.store
            .scan_all_term_ids()
            .into_iter()
            .filter(|(s, p, o)| !self.tombstones.contains(&(s.0, p.0, o.0)))
            .filter_map(|(s, p, o)| Some((dict.lookup(s)?, dict.lookup(p)?, dict.lookup(o)?)))
            .collect()
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

    /// Get-or-build the [`SnapshotStats`] summary for `snapshot`, caching it
    /// against the snapshot's `Arc` identity. Reuses the cached stats when they
    /// were built from the same snapshot `Arc`; otherwise rebuilds (a full
    /// snapshot scan) and replaces the cache. Correct across writes with no
    /// explicit invalidation: any mutation rebuilds the snapshot into a new
    /// `Arc`, which fails `Arc::ptr_eq` against the cached one.
    fn snapshot_stats(&self, snapshot: &Arc<VecTripleSource>) -> Arc<SnapshotStats> {
        let mut guard = self.stats_cache.lock().expect("stats cache lock poisoned");
        if let Some((cached_snap, cached_stats)) = guard.as_ref() {
            if Arc::ptr_eq(cached_snap, snapshot) {
                return Arc::clone(cached_stats);
            }
        }
        let stats = Arc::new(SnapshotStats::from_source(snapshot.as_ref()));
        *guard = Some((Arc::clone(snapshot), Arc::clone(&stats)));
        stats
    }

    /// Translate sparql `TriplePattern`s to WCOJ patterns for cardinality
    /// estimation.
    ///
    /// Simpler than `scan_bgp`'s translation: the estimator needs only each slot
    /// as a bound id or a per-name variable index. It needs no diagonal-alias
    /// handling (a variable repeated within one pattern just reuses that
    /// pattern's index — it is not "shared across patterns"), and no
    /// ground/non-ground split.
    ///
    /// Variable indices are assigned per distinct variable NAME across the BGP,
    /// in first-appearance order.
    ///
    /// Returns `Ok(wpatterns)`, or `Err(estimate)` to short-circuit:
    /// * `Err(0)` — a constant is unknown to the dictionary (or not
    ///   representable), so the BGP can match nothing.
    /// * `Err(self.len())` — the BGP has more than 256 distinct variables,
    ///   beyond the `WVar` (`u8`) index space; fall back to the coarse count.
    fn estimate_wpatterns(
        &self,
        patterns: &[TriplePattern],
    ) -> std::result::Result<Vec<WPattern>, usize> {
        let dict = self.store.dictionary();
        // SPARQL variable name -> WCOJ var index, first-appearance order.
        let mut var_index: HashMap<String, u8> = HashMap::new();
        let mut wpatterns: Vec<WPattern> = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            let mut slots = [WTerm::Var(WVar(0)); 3];
            let slot_terms = [&pattern.subject, &pattern.predicate, &pattern.object];
            for (slot_no, term) in slot_terms.into_iter().enumerate() {
                slots[slot_no] = match term {
                    Term::Var(v) => {
                        let name = v.name();
                        let idx = match var_index.get(name) {
                            Some(&i) => i,
                            None => {
                                let next = var_index.len();
                                if next > u8::MAX as usize {
                                    return Err(usize::try_from(self.len()).unwrap_or(usize::MAX));
                                }
                                var_index.insert(name.to_owned(), next as u8);
                                next as u8
                            }
                        };
                        WTerm::Var(WVar(idx))
                    }
                    constant => {
                        // Unrepresentable constants (variables can't occur here;
                        // RDF 1.2 triple terms aren't stored) match nothing.
                        let ox = algebra_to_oxrdf(constant).map_err(|_| 0usize)?;
                        match dict.get(&ox) {
                            Some(id) => WTerm::Bound(id.0),
                            None => return Err(0),
                        }
                    }
                };
            }
            wpatterns.push(WPattern::new(slots[0], slots[1], slots[2]));
        }
        Ok(wpatterns)
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
    fn clear_all(&mut self) {
        if self.live == 0 {
            return;
        }
        // Insertion-only storage: tombstone every physically-written key.
        // `stored_keys` never shrinks, so cloning it into `tombstones`
        // hides all live rows from `wcoj_snapshot` without touching the
        // columns. Re-inserting a triple later clears its tombstone via
        // `insert_oxrdf`/`insert_oxrdf_batch`, resurrecting it as usual.
        self.tombstones = self.stored_keys.clone();
        self.live = 0;
        self.invalidate();
    }
}

impl Executor for HornBackend {
    // keep in sync with scan_bgp_ids (its compilation loop is a verbatim copy of this one)
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
                            // The leading space guarantees freshness: SPARQL
                            // VARNAME can never contain U+0020, so this alias
                            // cannot collide with any parsed user variable.
                            // It lives only in the internal var table and the
                            // diagonal-filter list, and is stripped from rows
                            // before they leave scan_bgp.
                            let alias = format!(" dup_{name}_{slot_no}");
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

    /// Decode a dictionary id to its term.
    /// keep in sync with scan_bgp's dict.lookup + oxrdf_to_algebra call shape.
    fn decode_term(&self, id: TermId) -> Result<Term> {
        let ox = self
            .store
            .dictionary()
            .lookup(id)
            .ok_or_else(|| SparqlError::Executor(format!("dangling TermId {id:?}")))?;
        Ok(oxrdf_to_algebra(&ox))
    }

    /// Non-interning dictionary lookup used to canonicalize join keys. A term
    /// that does not convert to a storage term, or is absent from the
    /// dictionary, returns `None` (the caller keys it lexically). Inline-int
    /// literals always resolve (value-encoded, not dictionary-allocated).
    fn encode_term(&self, term: &Term) -> Option<TermId> {
        let ox = algebra_to_oxrdf(term).ok()?;
        self.store.dictionary().get(&ox)
    }

    /// Scan a BGP returning id-carrying slot rows without decoding TermId → String.
    /// The diagonal filter is applied inline by comparing raw ids; aliases are
    /// excluded from the output schema.
    // keep in sync with scan_bgp
    fn scan_bgp_ids(
        &self,
        patterns: &[crate::algebra::TriplePattern],
    ) -> Result<crate::exec::Batch> {
        use crate::algebra::Var;
        use crate::exec::{Batch, Row, Slot};

        if patterns.is_empty() {
            return Ok(Batch::unit());
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        // === VERBATIM copy from scan_bgp: pattern compilation ===
        let mut var_index: HashMap<String, u8> = HashMap::new();
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
                            let alias = format!(" dup_{name}_{slot_no}");
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
                            None => return Ok(Batch::empty()),
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

        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Batch::empty());
        }
        if wpatterns.is_empty() {
            return Ok(Batch::unit());
        }
        // === END verbatim copy ===

        // Output schema: var_index entries in ascending WVar (u8) order,
        // minus diagonal aliases (stripped from output like scan_bgp does).
        let aliases: HashSet<&str> = diagonal_filters.iter().map(|(_, a)| a.as_str()).collect();
        let mut ordered: Vec<(String, u8)> = var_index
            .iter()
            .filter(|(name, _)| !aliases.contains(name.as_str()))
            .map(|(n, i)| (n.clone(), *i))
            .collect();
        ordered.sort_by_key(|(_, i)| *i);
        let schema: Vec<Var> = ordered.iter().map(|(n, _)| Var::new(n.as_str())).collect();

        let bgp = WBgp::new(wpatterns);
        let mut rows: Vec<Row> = Vec::new();
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let arrow_schema = batch.schema();
            // Include ALL vars from var_index (including aliases) so the
            // diagonal check can compare original vs alias columns.
            let mut cols: Vec<(&str, &UInt64Array)> = Vec::with_capacity(var_index.len());
            for (name, idx) in &var_index {
                let Some((col_idx, _)) = arrow_schema.column_with_name(&format!("v{idx}")) else {
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
            // Precompute column indices once per batch (avoid an O(cols) search per row).
            let pos = |want: &str| cols.iter().position(|(n, _)| *n == want);
            let diag_col_idx: Vec<(usize, usize)> = diagonal_filters
                .iter()
                .filter_map(|(orig, alias)| Some((pos(orig)?, pos(alias)?)))
                .collect();
            let schema_col_idx: Vec<Option<usize>> = schema.iter().map(|v| pos(v.name())).collect();
            for r in 0..batch.num_rows() {
                // Diagonal filter: compare raw ids for alias pairs (no decode needed).
                // filter_map above drops any pair whose orig or alias column is absent —
                // that preserves the previous "missing column ⇒ no constraint" semantics.
                let keep = diag_col_idx
                    .iter()
                    .all(|&(io, ia)| cols[io].1.value(r) == cols[ia].1.value(r));
                if !keep {
                    continue;
                }
                let slots = schema_col_idx
                    .iter()
                    .map(|idx| match idx {
                        Some(i) => Slot::Id(TermId(cols[*i].1.value(r))),
                        None => Slot::Unbound,
                    })
                    .collect();
                rows.push(Row(slots));
            }
        }
        Ok(Batch { schema, rows })
    }

    /// Stats-backed point estimate of a BGP's output size, used by `EXPLAIN`.
    ///
    /// Returns the layered estimator's point estimate over recompute-from-snapshot
    /// statistics ([`SnapshotStats`] + [`StatsEstimator`]), replacing the old
    /// coarse live-triple-count upper bound. Special cases: the empty BGP is the
    /// join identity (`1`); an empty store or a constant unknown to the
    /// dictionary yields `0`; a BGP beyond the WVar index space falls back to the
    /// coarse live count.
    fn cardinality_estimate(&self, patterns: &[TriplePattern]) -> Option<usize> {
        // The empty BGP is the join identity: one row.
        if patterns.is_empty() {
            return Some(1);
        }
        let snapshot = self.wcoj_snapshot();
        // Empty store: no pattern can match.
        if snapshot.total_triples() == 0 {
            return Some(0);
        }
        let wpatterns = match self.estimate_wpatterns(patterns) {
            Ok(w) => w,
            // A short-circuit estimate the translation already resolved
            // (0 for an unknown constant, or the coarse live count as a
            // fallback when the BGP exceeds the WVar index space).
            Err(short_circuit) => return Some(short_circuit),
        };
        // Recompute-from-snapshot statistics (SPEC-23 Phase 3), fed to the
        // layered estimator. Building `SnapshotStats` scans the whole snapshot,
        // so cache it keyed on the snapshot's `Arc` identity: an `EXPLAIN` with
        // many BgpScan/GroupCountScan nodes calls this once per node, and every
        // node shares one snapshot. The cache self-invalidates because a write
        // rebuilds the snapshot into a new `Arc` that fails the `ptr_eq` check.
        let stats = self.snapshot_stats(&snapshot);
        let est = StatsEstimator::new(stats.as_ref());
        let e = est.estimate_bgp(&wpatterns);
        Some(usize::try_from(e.estimate).unwrap_or(usize::MAX))
    }

    /// Count BGP solutions without decoding terms or materializing rows.
    ///
    /// The count returned (when `Some`) is exactly the number of solution rows
    /// `scan_bgp_ids` would produce. It reuses the same pattern-compilation as
    /// `scan_bgp`/`scan_bgp_ids` (kept verbatim, like the existing copies), but
    /// instead of building `Row`s it sums the WCOJ batch row counts.
    ///
    /// One case falls back to the scan-and-count path (`Ok(None)`): a BGP with
    /// a variable repeated *within a single pattern* (e.g. `?s ?p ?s`). That
    /// needs a per-row "diagonal" filter to drop off-diagonal WCOJ rows, which
    /// cannot be done by a bare `num_rows()` sum. Returning `None` keeps the
    /// result correct via the caller's scan+len fallback.
    // keep in sync with scan_bgp_ids
    fn count_bgp(&self, patterns: &[TriplePattern]) -> Result<Option<usize>> {
        // The empty BGP is the join identity: one solution.
        if patterns.is_empty() {
            return Ok(Some(1));
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        let mut var_index: HashMap<String, u8> = HashMap::new();
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
                            let alias = format!(" dup_{name}_{slot_no}");
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
                            None => return Ok(Some(0)),
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

        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Some(0));
        }
        if wpatterns.is_empty() {
            // All patterns ground and present: one solution (ASK/unit).
            return Ok(Some(1));
        }

        // A within-pattern repeated variable needs the per-row diagonal filter;
        // a bare row-count sum would overcount. Fall back to scan+len.
        if !diagonal_filters.is_empty() {
            return Ok(None);
        }

        // No diagonal filter: every WCOJ row is one solution, so the solution
        // count is the sum of batch row counts — no decode, no Row build.
        let bgp = WBgp::new(wpatterns);
        let mut count: usize = 0;
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            count += batch.num_rows();
        }
        Ok(Some(count))
    }

    /// Per-group BGP solution counts without decoding terms or building rows:
    /// hash the raw u64 key columns of the WCOJ batches. Same fallback cases
    /// as `count_bgp` (diagonal repeats), plus: an all-ground BGP or a key
    /// with no WCOJ column returns `Ok(None)` so the caller's scan-based
    /// fallback supplies the (identical) semantics. Empty `patterns`/`keys`
    /// are the caller's job (`GroupCountScanOp` routes no-key shapes through
    /// `count_bgp`).
    // keep in sync with scan_bgp_ids
    fn count_bgp_grouped(
        &self,
        patterns: &[TriplePattern],
        keys: &[Var],
    ) -> Result<Option<Vec<GroupCount>>> {
        if patterns.is_empty() || keys.is_empty() {
            return Ok(None);
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        // === VERBATIM copy from scan_bgp: pattern compilation ===
        let mut var_index: HashMap<String, u8> = HashMap::new();
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
                            let alias = format!(" dup_{name}_{slot_no}");
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
                            // Unknown constant: no stored triple can match —
                            // zero groups (parity with the empty scan).
                            None => return Ok(Some(Vec::new())),
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

        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Some(Vec::new()));
        }
        // === END verbatim copy ===

        // All patterns ground (unit relation) — no key columns exist here;
        // let the scan-based fallback supply the Unbound-key semantics.
        if wpatterns.is_empty() {
            return Ok(None);
        }
        // A within-pattern repeated variable needs the per-row diagonal
        // filter, which a key-column hash cannot apply. Fall back.
        if !diagonal_filters.is_empty() {
            return Ok(None);
        }
        // Resolve each key's WCOJ var index; a key the BGP does not bind has
        // no column (the rewrite guards this; stay defensive).
        let mut key_wvars: Vec<u8> = Vec::with_capacity(keys.len());
        for k in keys {
            match var_index.get(k.name()) {
                Some(&i) => key_wvars.push(i),
                None => return Ok(None),
            }
        }

        let bgp = WBgp::new(wpatterns);
        let mut counts: HashMap<Vec<u64>, usize> = HashMap::new();
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let arrow_schema = batch.schema();
            let mut key_cols: Vec<&UInt64Array> = Vec::with_capacity(key_wvars.len());
            for idx in &key_wvars {
                let Some((col_idx, _)) = arrow_schema.column_with_name(&format!("v{idx}")) else {
                    // Executor produced no column for a key var — fall back
                    // wholesale rather than fabricate Unbound groups.
                    return Ok(None);
                };
                let arr = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .ok_or_else(|| {
                        SparqlError::Executor(format!("wcoj batch column v{idx} is not UInt64"))
                    })?;
                key_cols.push(arr);
            }
            for r in 0..batch.num_rows() {
                let key: Vec<u64> = key_cols.iter().map(|c| c.value(r)).collect();
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        Ok(Some(
            counts
                .into_iter()
                .map(|(ids, n)| (ids.into_iter().map(|id| Slot::Id(TermId(id))).collect(), n))
                .collect(),
        ))
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
    fn engine_key_literals_parse_raw_values() {
        // Embedded quotes and backslashes are raw, not escapes.
        match lexical_to_oxrdf(
            "\"a \"quoted\" \\ value\"^^<http://www.w3.org/2001/XMLSchema#string>",
        ) {
            OxTerm::Literal(l) => assert_eq!(l.value(), "a \"quoted\" \\ value"),
            other => panic!("expected literal, got {other:?}"),
        }
        // Lang form with a raw value that itself ends in something @-like.
        match lexical_to_oxrdf("\"x\"@de\"@en") {
            OxTerm::Literal(l) => {
                assert_eq!(l.value(), "x\"@de");
                assert_eq!(l.language(), Some("en"));
            }
            other => panic!("expected literal, got {other:?}"),
        }
        // Typed key whose raw value contains a full quoted-lang-looking chunk.
        match lexical_to_oxrdf("\"say \"hi\"@en\"^^<http://www.w3.org/2001/XMLSchema#string>") {
            OxTerm::Literal(l) => assert_eq!(l.value(), "say \"hi\"@en"),
            other => panic!("expected literal, got {other:?}"),
        }
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

    #[test]
    fn count_bgp_grouped_matches_scan_grouping() {
        use crate::algebra::TriplePattern;
        use crate::exec::{Executor, KeyPart, Slot};
        use std::collections::HashMap;
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // cat0: two works, cat1: one.
        b.insert_triple(iri("w0"), iri("cat"), iri("cat0"));
        b.insert_triple(iri("w1"), iri("cat"), iri("cat0"));
        b.insert_triple(iri("w2"), iri("cat"), iri("cat1"));
        let var = |n: &str| Term::Var(Var::new(n));
        let patterns = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("cat"),
            object: var("cat"),
        }];
        let keys = [Var::new("cat")];

        let fast = b
            .count_bgp_grouped(&patterns, &keys)
            .unwrap()
            .expect("HornBackend must provide a fast grouped count");

        // Oracle: group the id-rows scan_bgp_ids yields on the key column.
        let batch = b.scan_bgp_ids(&patterns).unwrap();
        let key_col = batch.col("cat").expect("?cat column");
        let mut want: HashMap<KeyPart, usize> = HashMap::new();
        for r in &batch.rows {
            *want.entry(r.0[key_col].key_part()).or_insert(0) += 1;
        }
        assert_eq!(fast.len(), want.len(), "one entry per group: {fast:?}");
        for (key_slots, n) in &fast {
            assert_eq!(key_slots.len(), 1);
            assert!(
                matches!(key_slots[0], Slot::Id(_)),
                "keys keep scan provenance (Slot::Id): {key_slots:?}"
            );
            assert_eq!(
                want.get(&key_slots[0].key_part()),
                Some(n),
                "count mismatch for {key_slots:?}"
            );
        }

        // A constant the dictionary has never seen: zero groups (matches the
        // empty scan), not None.
        let missing = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("nope"),
            object: var("cat"),
        }];
        assert_eq!(
            b.count_bgp_grouped(&missing, &keys).unwrap(),
            Some(Vec::new())
        );
    }

    #[test]
    fn count_bgp_grouped_falls_back_on_diagonal_and_unbound_key() {
        use crate::algebra::TriplePattern;
        use crate::exec::Executor;
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        b.insert_triple(iri("x"), iri("p"), iri("x"));
        let var = |n: &str| Term::Var(Var::new(n));
        // A var repeated within one pattern needs the per-row diagonal
        // filter, which a key-column hash cannot apply: fall back (None).
        let diag = vec![TriplePattern {
            subject: var("v"),
            predicate: iri("p"),
            object: var("v"),
        }];
        assert!(b
            .count_bgp_grouped(&diag, &[Var::new("v")])
            .unwrap()
            .is_none());
        // A key the BGP does not bind has no WCOJ column: fall back (None).
        let plain = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("p"),
            object: var("o"),
        }];
        assert!(b
            .count_bgp_grouped(&plain, &[Var::new("z")])
            .unwrap()
            .is_none());
    }
}
