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
use crate::exec::scope::{is_reserved_graph, per_graph_unsupported, ResolvedScope, ScanScope};
use crate::exec::{Bindings, Executor, GroupCount, Slot, Store};
use arrow::array::UInt64Array;
use horndb_storage::{GraphId, Store as ColumnStore, StoreSnapshot, TermId, DEFAULT_GRAPH};
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

/// A `(graph, subject, predicate, object)` TermId key. A named struct
/// instead of a positional `(u64, u64, u64, u64)` tuple: `g` is a
/// different *kind* of id from `s`/`p`/`o` (a graph id, not a term id),
/// and a bare tuple of four same-typed fields lets a transposed
/// construction pass the type checker silently.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct QuadKey {
    g: u64,
    s: u64,
    p: u64,
    o: u64,
}

impl QuadKey {
    fn new(g: GraphId, s: TermId, p: TermId, o: TermId) -> Self {
        Self {
            g: g.0,
            s: s.0,
            p: p.0,
            o: o.0,
        }
    }
}

/// A [`ScanScope`](crate::exec::ScanScope) resolved against *this* store's
/// dictionary: the graph ids a WCOJ snapshot is built from. Doubles as the
/// snapshot memo's key, so equal scopes share one built source.
///
/// [`ResolvedScope::PerGraph`] has no variant here on purpose: `GRAPH ?g`
/// binds a per-row graph column rather than reading one flattened source
/// (SPEC-28 D6), so it does not resolve to a snapshot at all. It is refused
/// at [`HornBackend::resolve_scope`] until PLAN-28-03 Task 4 lands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SnapshotScope {
    /// Every non-reserved graph, deduped (D2 `union`).
    DefaultUnion,
    /// The default-graph sentinel alone (D2 `strict`).
    DefaultStrict,
    /// Deduped set union of exactly these graphs, sorted for a stable memo
    /// key. Empty = the empty graph (an unknown or dataset-excluded graph
    /// name lands here, which is how "unknown graph ⇒ zero rows, not an
    /// error" is implemented).
    FromUnion(Vec<GraphId>),
    /// Exactly one graph — no dedup needed, since a graph holds a triple at
    /// most once.
    OneGraph(GraphId),
}

impl SnapshotScope {
    /// Whether a built snapshot for this scope is worth caching — see
    /// [`HornBackend::wcoj_snapshot`] for the reasoning.
    fn memoisable(&self) -> bool {
        matches!(
            self,
            SnapshotScope::DefaultUnion | SnapshotScope::DefaultStrict
        )
    }
}

/// The empty graph: zero rows, no error. (Distinct from `scope.rs`'s
/// `EMPTY_GRAPH_SET`, which is the *name* list this resolves from.)
const EMPTY_GRAPH_SCOPE: SnapshotScope = SnapshotScope::FromUnion(Vec::new());

/// True if `g` is a HornDB-internal graph (SPEC-27 F6 / SPEC-29 D4). The
/// default-graph sentinel has no IRI (`graph_uri` errors on it) and is never
/// reserved, so it stays in the union default graph.
fn reserved_graph(snap: &StoreSnapshot<'_>, g: GraphId) -> bool {
    match snap.graph_uri(g) {
        Ok(OxTerm::NamedNode(n)) => is_reserved_graph(n.as_str()),
        _ => false,
    }
}

/// Every id-triple in one graph. `graph_len` is O(predicates in `g`), so
/// sizing the vector up front costs nothing and avoids the doubling regrowth
/// a `FlatMap` collect would pay (its `size_hint` is `(0, None)`).
fn graph_triples(snap: &StoreSnapshot<'_>, g: GraphId) -> Vec<WTriple> {
    let mut out = Vec::with_capacity(snap.graph_len(g));
    out.extend(
        snap.iter_graph_term_ids(g)
            .map(|(s, p, o)| WTriple::new(s.0, p.0, o.0)),
    );
    out
}

/// Union of `graphs`' id-triples, concatenated.
///
/// No dedup pass here: `VecTripleSource::from_triples` sorts and dedups each
/// of its six orderings unconditionally, so the snapshot has set semantics
/// however many copies of a triple it is handed — which is what SPEC-28 S3's
/// union default graph requires. The one thing a pre-pass would buy is a
/// truthful `VecTripleSource::total_triples()`, which over-counts under a
/// multi-graph union; it reaches only `cardinality_estimate`'s `== 0` check
/// (still correct — an empty input is still zero) and the WCOJ estimators,
/// never a result row.
fn union_triples(snap: &StoreSnapshot<'_>, graphs: &[GraphId]) -> Vec<WTriple> {
    match graphs {
        [] => Vec::new(),
        [g] => graph_triples(snap, *g),
        many => {
            let mut out = Vec::with_capacity(many.iter().map(|g| snap.graph_len(*g)).sum());
            for g in many {
                out.extend(
                    snap.iter_graph_term_ids(*g)
                        .map(|(s, p, o)| WTriple::new(s.0, p.0, o.0)),
                );
            }
            out
        }
    }
}

/// Storage + WCOJ backed SPARQL backend (issue #67).
///
/// * Term identity: `horndb_storage::Dictionary` (kind-tagged TermIds).
/// * Reads: Leapfrog Triejoin over a lazily-built [`VecTripleSource`]
///   snapshot (all six orderings, rebuilt after any mutation — a
///   documented Stage-1 cost; see INTEGRATION-NOTES.md).
/// * Writes: `DELETE DATA` and `CLEAR`/`DROP` retract through
///   `horndb_storage::Store`'s native per-tuple MVCC delete path
///   (SPEC-25 S1) — a retracted tuple's `end` stamp is set, and every
///   store read (`scan_all_term_ids`, `triple_count`, …) is already
///   visibility-filtered, so `HornBackend` needs no overlay.
///
/// RDF term identity is preserved: canonical-form `xsd:integer`
/// literals (e.g. `"42"`) use the dictionary's inline-int fast path,
/// while non-canonical lexical forms (`"042"`, `"+42"`) keep distinct
/// dictionary identities and round-trip their exact lexical form.
/// Matching is therefore term-based (lexical form + datatype), as
/// SPARQL BGP semantics require.
pub struct HornBackend {
    store: ColumnStore,
    /// Mirror of every `(graph, s, p, o)` TermId key currently LIVE in
    /// `store` (inserted on insert, removed on retract), keyed by *quad* —
    /// the same triple in two graphs is two entries (SPEC-28 S2). Gives O(1)
    /// membership tests for `INSERT DATA` idempotency and `DELETE DATA`
    /// no-op detection, avoiding storage's O(partition-size)
    /// `StoreSnapshot::contains` on the bulk-load hot path. See
    /// `insert_oxrdf_in_graph` for the write funnel's current graph scope.
    live_keys: HashSet<QuadKey>,
    /// Lazily-built WCOJ sources, one per [`SnapshotScope`] a query has
    /// asked for. Cleared wholesale after any mutation. Most workloads use
    /// one entry (the unqualified default graph); a query mixing `GRAPH`
    /// scopes adds one per distinct scope.
    snapshots: Mutex<HashMap<SnapshotScope, Arc<VecTripleSource>>>,
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
            live_keys: HashSet::new(),
            snapshots: Mutex::new(HashMap::new()),
            stats_cache: Mutex::new(None),
        }
    }

    /// Live triple count across every graph, visibility-filtered (SPEC-25 S1).
    /// (SPEC-28 phase 3 revisits the union default graph.)
    pub fn len(&self) -> u64 {
        self.store.triple_count()
    }

    /// Cheap point-in-time size stats for scrape-time metrics: live triple
    /// count plus the tier's already-tracked graph/predicate/byte estimates and
    /// the dictionary term count. Bounded by the number of distinct
    /// predicates/graphs — never an O(triples) traversal. `tier.triples` is
    /// visibility-filtered by the tier itself, so no adjustment is needed here.
    pub fn storage_stats(&self) -> HornStorageStats {
        let tier = self.store.stats();
        HornStorageStats {
            triples: tier.triples,
            graphs: tier.graphs,
            predicates: tier.predicates,
            dictionary_terms: self.store.dictionary().len() as u64,
            bytes_estimated: tier.bytes_estimated,
        }
    }

    /// Whole-store emptiness — see [`Self::len`] for scope.
    pub fn is_empty(&self) -> bool {
        self.store.triple_count() == 0
    }

    fn invalidate(&mut self) {
        self.snapshots
            .get_mut()
            .expect("snapshot lock poisoned")
            .clear();
        // Clear the stats cache too: releases the obsolete snapshot's Arc (all six
        // sorted indexes) immediately rather than pinning it until the next estimate.
        *self
            .stats_cache
            .get_mut()
            .expect("stats_cache lock poisoned") = None;
    }

    /// Insert one oxrdf triple into the default graph. Returns true if it was
    /// new (i.e. live count increased).
    pub fn insert_oxrdf(
        &mut self,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        self.insert_oxrdf_in_graph(DEFAULT_GRAPH, s, p, o)
    }

    /// Insert one oxrdf triple into the named graph `graph`, interning the
    /// graph name first. Returns true if the quad was new.
    ///
    /// The seam SPEC-28 phase 3 needs to seed named graphs. The SPARQL
    /// `Store` write trait stays triple-shaped and default-graph scoped;
    /// the named-graph *update* path (`INSERT DATA { GRAPH … }`, GSP) is
    /// phase 4 (#267).
    pub fn insert_oxrdf_in_named_graph(
        &mut self,
        graph: &oxrdf::Term,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        let g = self
            .store
            .intern_graph_uri(graph)
            .map_err(|e| SparqlError::Executor(format!("intern graph: {e}")))?;
        self.insert_oxrdf_in_graph(g, s, p, o)
    }

    /// Insert one oxrdf triple into graph `g`.
    ///
    /// Precondition: `g` must already have been interned via
    /// `Store::intern_graph_uri` — `horndb_storage::Store::insert_quads`
    /// requires it and cannot decode an ad-hoc `GraphId`.
    /// [`Self::insert_oxrdf_in_named_graph`] is the interning entry point.
    fn insert_oxrdf_in_graph(
        &mut self,
        g: GraphId,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<bool> {
        let key = self.intern_key(g, s, p, o)?;
        if self.live_keys.contains(&key) {
            return Ok(false); // SPARQL INSERT DATA is idempotent on an already-live triple
        }
        self.store
            .insert_quads(&[(g, s.clone(), p.clone(), o.clone())])
            .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
        self.live_keys.insert(key);
        self.invalidate();
        Ok(true)
    }

    /// Bulk-insert oxrdf triples in one storage batch. Returns the number of
    /// newly-live triples. Delegates to the graph-scoped internal funnel with
    /// `DEFAULT_GRAPH`; see `insert_oxrdf_batch_in_graph` for the algorithm.
    pub fn insert_oxrdf_batch(
        &mut self,
        triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)>,
    ) -> Result<u64> {
        self.insert_oxrdf_batch_in_graph(DEFAULT_GRAPH, triples)
    }

    /// Bulk-insert oxrdf triples into graph `g` in one storage batch. `g`
    /// must already be interned (see [`Self::insert_oxrdf_in_graph`]); no
    /// bulk named-graph entry point exists yet, so today's only caller
    /// passes `DEFAULT_GRAPH`. Same idempotency semantics as
    /// `insert_oxrdf`; the columnar tier rebuilds each predicate partition at
    /// most once, and the snapshots are invalidated once at the end.
    ///
    /// Uses a read-compute / write-commit split to keep the storage insert
    /// correct even when intern errors occur:
    ///
    /// * Phase 1 (read-only): intern all terms and drop any triple already
    ///   live (via `live_keys`, an O(1) check) or repeated within this batch.
    ///   Any intern failure skips that triple.
    /// * Phase 2 (write): call `store.insert_quads` once for the surviving
    ///   entries, then mark them live only on success. Propagates storage
    ///   errors.
    fn insert_oxrdf_batch_in_graph(
        &mut self,
        g: GraphId,
        triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)>,
    ) -> Result<u64> {
        if triples.is_empty() {
            return Ok(0);
        }

        // Phase 1 (read-only): intern and drop already-live/intra-batch-duplicate
        // triples. `intra_batch` deduplicates within the batch itself in O(1)
        // per triple.
        struct Entry {
            key: QuadKey,
            ox: (oxrdf::Term, oxrdf::Term, oxrdf::Term),
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(triples.len());
        let mut intra_batch: HashSet<QuadKey> = HashSet::new();
        {
            let d = self.store.dictionary();
            for (s, p, o) in triples {
                let (si, pi, oi) = match (d.intern(&s), d.intern(&p), d.intern(&o)) {
                    (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                    _ => continue, // intern failure — skip this triple (lenient for bulk loads; the single-triple insert_oxrdf propagates instead)
                };
                let key = QuadKey::new(g, si, pi, oi);
                if self.live_keys.contains(&key) {
                    continue; // already live — no-op
                }
                if !intra_batch.insert(key) {
                    continue; // duplicate within this batch; first occurrence wins
                }
                entries.push(Entry { key, ox: (s, p, o) });
            }
        }

        if entries.is_empty() {
            return Ok(0);
        }

        // Phase 2 (write): storage insert first, then bookkeeping. `entries`
        // is dead after the move below except for `e.key` (already extracted
        // into `keys`, and `Copy`), so this moves each triple's terms into
        // the storage call instead of cloning them.
        let keys: Vec<QuadKey> = entries.iter().map(|e| e.key).collect();
        let to_store: Vec<(GraphId, oxrdf::Term, oxrdf::Term, oxrdf::Term)> = entries
            .into_iter()
            .map(|e| (g, e.ox.0, e.ox.1, e.ox.2))
            .collect();
        self.store
            .insert_quads(&to_store)
            .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;

        for key in keys {
            self.live_keys.insert(key);
        }
        self.invalidate();

        Ok(to_store.len() as u64)
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
        g: GraphId,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Result<QuadKey> {
        let d = self.store.dictionary();
        let err = |e: horndb_storage::StorageError| SparqlError::Executor(format!("intern: {e}"));
        Ok(QuadKey::new(
            g,
            d.intern(s).map_err(err)?,
            d.intern(p).map_err(err)?,
            d.intern(o).map_err(err)?,
        ))
    }

    /// Materialize every live triple as oxrdf terms, for export /
    /// serialization (the server never dumps outside tests, so this is the
    /// read-back seam). `store.scan_all_term_ids()` is already
    /// visibility-filtered (SPEC-25 S1); any triple whose TermIds no longer
    /// resolve in the dictionary is silently dropped (cannot happen for an
    /// append-only dictionary).
    pub fn iter_oxrdf(&self) -> Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)> {
        let dict = self.store.dictionary();
        self.store
            .scan_all_term_ids()
            .into_iter()
            .filter_map(|(s, p, o)| Some((dict.lookup(s)?, dict.lookup(p)?, dict.lookup(o)?)))
            .collect()
    }

    /// The `GraphId` an IRI names, or `None` if the dictionary has never
    /// seen it. A **non-interning** lookup: a read must not mutate the
    /// dictionary, and an IRI naming no graph simply matches nothing.
    fn graph_id(&self, iri: &str) -> Option<GraphId> {
        let ox = OxTerm::NamedNode(NamedNode::new_unchecked(iri));
        self.store.dictionary().get(&ox).map(|t| GraphId(t.0))
    }

    /// Resolve a plan-level scan scope against this store's dictionary.
    ///
    /// Unknown graph names collapse to [`EMPTY_GRAPH_SCOPE`] (zero rows), per
    /// SPEC-28 S3. `GRAPH ?g` is refused — see [`SnapshotScope`].
    fn resolve_scope(&self, scope: &ScanScope<'_>) -> Result<SnapshotScope> {
        Ok(match scope.resolve() {
            ResolvedScope::DefaultUnion => SnapshotScope::DefaultUnion,
            ResolvedScope::DefaultStrict => SnapshotScope::DefaultStrict,
            ResolvedScope::OneGraph(iri) => match self.graph_id(iri) {
                Some(g) => SnapshotScope::OneGraph(g),
                None => EMPTY_GRAPH_SCOPE,
            },
            ResolvedScope::Union(iris) => {
                let mut ids: Vec<GraphId> = iris.iter().filter_map(|i| self.graph_id(i)).collect();
                ids.sort_by_key(|g| g.0);
                ids.dedup();
                match ids.len() {
                    1 => SnapshotScope::OneGraph(ids[0]),
                    _ => SnapshotScope::FromUnion(ids),
                }
            }
            ResolvedScope::PerGraph(v) => return Err(per_graph_unsupported(v)),
        })
    }

    /// Get-or-build the WCOJ snapshot for `scope`.
    ///
    /// **Only the two whole-store scopes are memoised.** They cost O(store)
    /// to build and every unqualified query wants one, so caching them is
    /// the pre-SPEC-28 behaviour preserved. A graph-scoped build is
    /// O(that graph), so caching it would buy little and cost a cache with
    /// no ceiling: one `Arc<VecTripleSource>` — six sorted index copies —
    /// per graph ever named, evicted only by a write, and reachable from an
    /// unauthenticated `/query` (`EXPLAIN` populates it without executing
    /// anything). A client walking `GRAPH <g1>`…`GRAPH <gN>` would pin ~6×
    /// the store. See `graph_scoped_snapshots_are_not_memoised`.
    ///
    /// The memo is dropped wholesale on any write ([`Self::invalidate`]).
    fn wcoj_snapshot(&self, scope: &SnapshotScope) -> Arc<VecTripleSource> {
        if !scope.memoisable() {
            return Arc::new(VecTripleSource::from_triples(self.scope_triples(scope)));
        }
        {
            let guard = self.snapshots.lock().expect("snapshot lock poisoned");
            if let Some(s) = guard.get(scope) {
                return Arc::clone(s);
            }
        }
        // Build with the lock RELEASED: six sort passes over the whole store
        // must not stall a concurrent reader whose own scope is already
        // cached (readers do run concurrently — `server/query.rs`). A race
        // duplicates the build and `or_insert` keeps the first result; the
        // two are interchangeable, since a write needs `&mut self` and so
        // cannot interleave with any read.
        let built = Arc::new(VecTripleSource::from_triples(self.scope_triples(scope)));
        let mut guard = self.snapshots.lock().expect("snapshot lock poisoned");
        Arc::clone(guard.entry(scope.clone()).or_insert(built))
    }

    /// Every `(s, p, o)` id-triple visible in `scope`, from one pinned store
    /// snapshot. Multi-graph scopes are a **set** union — the same triple in
    /// two graphs is one row of the union graph (SPEC-28 S3) — enforced by
    /// the snapshot builder's dedup; see [`union_triples`].
    fn scope_triples(&self, scope: &SnapshotScope) -> Vec<WTriple> {
        let snap = self.store.snapshot();
        match scope {
            SnapshotScope::DefaultStrict => graph_triples(&snap, DEFAULT_GRAPH),
            SnapshotScope::OneGraph(g) => graph_triples(&snap, *g),
            SnapshotScope::FromUnion(graphs) => union_triples(&snap, graphs),
            SnapshotScope::DefaultUnion => {
                // Recomputed on each memo miss, which is exactly when the
                // graph set may have changed (a write clears the memo).
                let graphs: Vec<GraphId> = snap
                    .graphs()
                    .into_iter()
                    .filter(|g| !reserved_graph(&snap, *g))
                    .collect();
                union_triples(&snap, &graphs)
            }
        }
    }

    /// Number of memoised snapshots. Test-only window on the cache that
    /// `graph_scoped_snapshots_are_not_memoised` bounds.
    #[cfg(test)]
    fn memo_len(&self) -> usize {
        self.snapshots.lock().expect("snapshot lock poisoned").len()
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
        let key = {
            let d = self.store.dictionary();
            // Non-interning lookups: a term the dictionary has never seen
            // cannot participate in any stored triple.
            let (Some(sid), Some(pid), Some(oid)) = (d.get(&s), d.get(&p), d.get(&o)) else {
                return;
            };
            // #267: needs the graph-threaded twin — this hardcodes
            // DEFAULT_GRAPH while the insert funnel is already graph-aware.
            QuadKey::new(DEFAULT_GRAPH, sid, pid, oid)
        };
        if !self.live_keys.remove(&key) {
            return; // not currently live — no-op (unknown or already deleted)
        }
        // Retract through native storage (SPEC-25 S1): stamps the matching
        // live row's `end`, the tuple stays physically present as history.
        let _ = self.store.retract_triples(&[(s, p, o)]);
        self.invalidate();
    }
    // TODO(#267): once a public write path can put data in a named graph,
    // `CLEAR DEFAULT`/`DROP DEFAULT` must stop routing to this whole-store
    // sweep (see the TODO in `crate::update::apply_clear_drop`).
    fn clear_all(&mut self) {
        // Consult the store, not the cache, for the early-out: `live_keys`
        // only ever holds entries the public write funnel inserted
        // (DEFAULT_GRAPH today), so a store that holds only named-graph data
        // planted below the funnel would have an empty `live_keys` and skip
        // the sweep entirely if this checked the cache instead.
        if self.store.triple_count() == 0 {
            return;
        }
        // Retract every currently-live quad in every graph through the
        // native storage delete path (SPEC-28 S2: `clear_all` is whole-store,
        // not default-graph-scoped). Re-inserting a triple afterward goes
        // through `insert_oxrdf`/`insert_oxrdf_batch` as usual, which stamps
        // a fresh live row (resurrection).
        let snapshot = self.store.snapshot();
        let snap = &snapshot;
        let quads: Vec<_> = snap
            .graphs()
            .into_iter()
            .flat_map(move |g| {
                snap.iter_graph_term_ids(g)
                    .map(move |(s, p, o)| (g, s, p, o))
            })
            .collect();
        let _ = self.store.tier().retract_quad_batch(&quads);
        self.live_keys.clear();
        self.invalidate();
    }
}

impl Executor for HornBackend {
    // keep in sync with scan_bgp_ids (its compilation loop is a verbatim copy of this one)
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        // Resolve the scope even for the empty BGP, so an unsupported scope
        // (`GRAPH ?g`) refuses uniformly instead of depending on the shape.
        let resolved = self.resolve_scope(scope)?;
        // The empty BGP is the unit of join: exactly one empty solution
        // (parity with MemStore and the SPARQL algebra).
        if patterns.is_empty() {
            return Ok(Box::new(std::iter::once(Bindings::new())));
        }

        let snapshot = self.wcoj_snapshot(&resolved);
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
        scope: &ScanScope<'_>,
    ) -> Result<crate::exec::Batch> {
        use crate::algebra::Var;
        use crate::exec::{Batch, Row, Slot};

        let resolved = self.resolve_scope(scope)?;
        if patterns.is_empty() {
            return Ok(Batch::unit());
        }

        let snapshot = self.wcoj_snapshot(&resolved);
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
    fn cardinality_estimate(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Option<usize> {
        // The empty BGP is the join identity: one row.
        if patterns.is_empty() {
            return Some(1);
        }
        // A scope with no snapshot form (`GRAPH ?g`) is simply "unknown".
        let snapshot = self.wcoj_snapshot(&self.resolve_scope(scope).ok()?);
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
    fn count_bgp(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Result<Option<usize>> {
        // Scope first: every count below is a count *within the scoped
        // snapshot*, so an unsupported scope must refuse here rather than
        // fall through to a wider count (SPEC-28 S3).
        let resolved = self.resolve_scope(scope)?;
        // The empty BGP is the join identity: one solution.
        if patterns.is_empty() {
            return Ok(Some(1));
        }

        let snapshot = self.wcoj_snapshot(&resolved);
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
        scope: &ScanScope<'_>,
    ) -> Result<Option<Vec<GroupCount>>> {
        // See `count_bgp`: resolve the scope before any counting.
        let resolved = self.resolve_scope(scope)?;
        if patterns.is_empty() || keys.is_empty() {
            return Ok(None);
        }

        let snapshot = self.wcoj_snapshot(&resolved);
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

    /// The snapshot memo must not grow with the number of graphs a client
    /// names. Walking N distinct `GRAPH <gi>` scopes used to leave N cached
    /// `VecTripleSource`s (six sorted indexes each), evicted only by a
    /// write — an unbounded, unauthenticated memory sink. Only the two
    /// whole-store scopes are cached now.
    #[test]
    fn graph_scoped_snapshots_are_not_memoised() {
        use crate::algebra::GraphSpec;
        use crate::plan::GraphScope;

        let mut b = HornBackend::new();
        let iri = |v: &str| OxTerm::NamedNode(NamedNode::new_unchecked(v));
        for i in 0..20 {
            b.insert_oxrdf_in_named_graph(
                &iri(&format!("http://ex/g{i}")),
                &iri(&format!("http://ex/s{i}")),
                &iri("http://ex/p"),
                &iri("http://ex/o"),
            )
            .unwrap();
        }
        let patterns = vec![TriplePattern {
            subject: Term::Var(Var::new("s")),
            predicate: Term::Iri("http://ex/p".into()),
            object: Term::Var(Var::new("o")),
        }];

        // One unqualified scan warms the single memoisable entry.
        let _ = b.scan_bgp_ids(&patterns, &ScanScope::DEFAULT).unwrap();
        assert_eq!(b.memo_len(), 1, "the union default graph is cached");

        // Twenty distinct ground scopes must add nothing, and must still
        // return the right rows (bounded ≠ broken).
        let dataset = crate::algebra::DatasetSpec::default();
        for i in 0..20 {
            let scope = GraphScope::Named(GraphSpec::Iri(format!("http://ex/g{i}")));
            let scope = ScanScope::new(&scope, &dataset, crate::DefaultGraphMode::Union);
            let batch = b.scan_bgp_ids(&patterns, &scope).unwrap();
            assert_eq!(batch.rows.len(), 1, "graph g{i} holds exactly one triple");
        }
        assert_eq!(
            b.memo_len(),
            1,
            "graph-scoped snapshots must not accumulate in the memo"
        );
    }

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
        // Re-insert after delete resurrects the triple (storage stamps a fresh live row).
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
    fn bulk_resurrect_of_deleted_triple_refreshes_snapshot() {
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
        let _ = b.wcoj_snapshot(&SnapshotScope::DefaultStrict); // warm: snapshot now has 0 triples
        b.insert_algebra_triples_bulk(vec![(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o".into()),
        )]);
        assert_eq!(b.len(), 1);
        assert_eq!(
            b.wcoj_snapshot(&SnapshotScope::DefaultStrict)
                .total_triples(),
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
        let _ = b.wcoj_snapshot(&SnapshotScope::DefaultStrict); // warm the cache
        b.delete_triple(
            &Term::Iri("http://ex/s".into()),
            &Term::Iri("http://ex/p".into()),
            &Term::Iri("http://ex/o".into()),
        );
        assert_eq!(b.len(), 0);
        let snap = b.wcoj_snapshot(&SnapshotScope::DefaultStrict);
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
            .count_bgp_grouped(&patterns, &keys, &ScanScope::DEFAULT)
            .unwrap()
            .expect("HornBackend must provide a fast grouped count");

        // Oracle: group the id-rows scan_bgp_ids yields on the key column.
        let batch = b.scan_bgp_ids(&patterns, &ScanScope::DEFAULT).unwrap();
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
            b.count_bgp_grouped(&missing, &keys, &ScanScope::DEFAULT)
                .unwrap(),
            Some(Vec::new())
        );
    }

    #[test]
    fn clear_all_sweeps_named_graphs() {
        let mut b = HornBackend::new();
        // #267.
        let g = b
            .store
            .intern_graph_uri(&OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/g")))
            .unwrap();
        b.insert_oxrdf_in_graph(
            g,
            &OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/s")),
            &OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/p")),
            &OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/o")),
        )
        .unwrap();
        // One default-graph triple through the backend proper.
        b.insert_triple(
            Term::Iri("http://ex/s2".into()),
            Term::Iri("http://ex/p2".into()),
            Term::Iri("http://ex/o2".into()),
        );
        assert_eq!(b.len(), 2);

        b.clear_all();

        assert!(
            b.is_empty(),
            "clear_all must sweep named graphs too, not just the default graph"
        );
        assert!(
            b.store.snapshot().graphs().is_empty(),
            "SPEC-28 D11: a fully-retracted graph must cease to exist"
        );
        assert!(b.live_keys.is_empty());
    }

    #[test]
    fn clear_all_sweeps_a_store_with_no_funnel_writes() {
        let mut b = HornBackend::new();
        // Plant a named-graph quad directly at the storage layer, bypassing
        // HornBackend's write funnel entirely, so `live_keys` stays empty on
        // entry. This is the case `clear_all`'s early-out must not skip:
        // consulting `live_keys.is_empty()` instead of `store.triple_count()`
        // would return here and leave the quad live (#265).
        let g = b
            .store
            .intern_graph_uri(&OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/g")))
            .unwrap();
        b.store
            .insert_quads(&[(
                g,
                OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/s")),
                OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/p")),
                OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/o")),
            )])
            .unwrap();
        assert!(
            b.live_keys.is_empty(),
            "planted below the funnel: live_keys must stay empty"
        );
        assert_eq!(b.len(), 1);

        b.clear_all();

        assert!(b.is_empty());
        assert!(b.store.snapshot().graphs().is_empty());
        assert!(b.live_keys.is_empty());
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
            .count_bgp_grouped(&diag, &[Var::new("v")], &ScanScope::DEFAULT)
            .unwrap()
            .is_none());
        // A key the BGP does not bind has no WCOJ column: fall back (None).
        let plain = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("p"),
            object: var("o"),
        }];
        assert!(b
            .count_bgp_grouped(&plain, &[Var::new("z")], &ScanScope::DEFAULT)
            .unwrap()
            .is_none());
    }
}
