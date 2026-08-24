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
use crate::exec::scope::{
    is_reserved_graph, per_graph_needs_the_scan_loop, NamedGraph, ResolvedScope, ScanScope,
};
use crate::exec::{
    AlgebraQuad, AlgebraTriple, ApplyCounts, Bindings, Executor, GroupCount, Slot, Store,
};
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
/// (SPEC-28 D6). The scan operator loops over the graphs and calls back in
/// with a `OneGraph` scope per graph, so a per-graph scope never reaches
/// [`HornBackend::resolve_scope`] — which refuses it if it somehow does.
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

/// A delta touching more than `1 / SNAPSHOT_DELTA_REBUILD_DIVISOR` of a cached
/// snapshot's rows is not worth merging in place: at that size a full rebuild
/// from the store costs about the same and is simpler, so the memo is dropped
/// instead. See [`HornBackend::apply_delta_to_snapshots`].
const SNAPSHOT_DELTA_REBUILD_DIVISOR: usize = 2;

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
///   snapshot (all six orderings). A small `apply_quads` delta is merged
///   into the cached snapshot in place; every other write drops the memo
///   and the next read rebuilds it — a documented Stage-1 cost, see
///   INTEGRATION-NOTES.md and [`HornBackend::apply_delta_to_snapshots`].
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
    /// asked for. A small `apply_quads` delta is merged into these in place
    /// ([`Self::apply_delta_to_snapshots`]); every other write clears them
    /// wholesale ([`Self::invalidate`]). Most workloads use one entry (the
    /// unqualified default graph); a query mixing `GRAPH` scopes adds one
    /// per distinct scope.
    snapshots: Mutex<HashMap<SnapshotScope, Arc<VecTripleSource>>>,
    /// Cached statistics summary derived from a specific snapshot, used by
    /// `EXPLAIN`'s `cardinality_estimate`. Holds the `Arc<VecTripleSource>` the
    /// stats were built from alongside the stats themselves, and is reused only
    /// while that `Arc` is still the current snapshot (`Arc::ptr_eq`).
    ///
    /// **Pointer identity alone does not prove freshness.** A delta merge
    /// mutates a snapshot through `Arc::get_mut`, which keeps the same pointer,
    /// so a stale entry would still pass the `ptr_eq` check. The invariant is
    /// therefore held by explicit clearing, not by identity: every write path
    /// clears this cache unconditionally — [`Self::invalidate`] and
    /// [`Self::apply_delta_to_snapshots`] both do. Clearing it *before* a merge
    /// is also what lets `Arc::get_mut` succeed at all: the cached `Arc` is a
    /// second strong reference to the very snapshot being merged into.
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

    /// Push a committed quad delta into every memoised snapshot, falling back
    /// to a full [`Self::invalidate`] whenever the delta cannot be applied
    /// safely or profitably.
    ///
    /// Rebuilding a whole-store snapshot means six sort passes over every
    /// triple, which dominates the cost of a small `SPARQL Update`. Merging the
    /// delta into the sorted orderings instead is `O(n + k log k)` per ordering
    /// and keeps the cache warm ([`VecTripleSource::apply_delta`]).
    ///
    /// Falling back is always correct, so every case this cannot *prove* is a
    /// fallback. The delta path is taken only when all of these hold:
    ///
    /// 1. The delta is small — no more than `1 /
    ///    SNAPSHOT_DELTA_REBUILD_DIVISOR` of the snapshot's rows. A bigger one
    ///    is no cheaper to merge than to rebuild.
    /// 2. Nobody else holds the snapshot, so `Arc::get_mut` can hand out the
    ///    `&mut` an in-place merge needs.
    /// 3. Every add interns. (It already did inside `apply_quads`, so this is a
    ///    dictionary hit; a failure would silently drop a row from the delta.)
    /// 4. For [`SnapshotScope::DefaultUnion`], the union covers exactly one
    ///    non-reserved graph *and* the whole delta lands in that graph. Two
    ///    conditions ride on this. The union default graph is a **set** union
    ///    (SPEC-28 S3), so with several graphs a delete from one must not drop
    ///    the union row while another graph still holds the same triple — the
    ///    per-row delta carries no such multiplicity. And a write to a graph
    ///    outside the union changes *which* graphs the union covers, which no
    ///    row-level delta can express. One graph — the shape of every
    ///    default-graph-only workload — has neither problem.
    ///
    /// [`SnapshotScope::DefaultStrict`] needs no such check: it reads one fixed
    /// graph, so rows in any other graph simply do not apply to it and are
    /// filtered out.
    fn apply_delta_to_snapshots(
        &mut self,
        del_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
        add_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
    ) {
        // First, unconditionally: the stats cache holds a second `Arc` to a live
        // snapshot and reuses its entry on pointer identity. An in-place merge
        // breaks both halves of that — the extra strong reference would fail
        // `Arc::get_mut` below, and the pointer does not change, so a stale
        // entry would still pass `Arc::ptr_eq`. See the field's doc.
        *self
            .stats_cache
            .get_mut()
            .expect("stats_cache lock poisoned") = None;

        // Each memoised scope with the row count it currently holds.
        let cached: Vec<(SnapshotScope, usize)> = {
            let guard = self.snapshots.lock().expect("snapshot lock poisoned");
            guard
                .iter()
                .map(|(scope, src)| (scope.clone(), src.total_triples()))
                .collect()
        };
        if cached.is_empty() {
            return;
        }

        // Resolve both sides to (graph, id-triple). A del row carrying a term
        // the dictionary has never seen cannot be live anywhere, so it drops
        // out of the delta; an add row that failed to intern would leave the
        // delta incomplete, so it forces the rebuild instead.
        let dels: Vec<(GraphId, WTriple)> = del_rows
            .iter()
            .filter_map(|(g, s, p, o)| {
                self.lookup_key(*g, s, p, o)
                    .map(|k| (*g, WTriple::new(k.s, k.p, k.o)))
            })
            .collect();
        let mut adds: Vec<(GraphId, WTriple)> = Vec::with_capacity(add_rows.len());
        for (g, s, p, o) in add_rows {
            let Ok(k) = self.intern_key(*g, s, p, o) else {
                self.invalidate();
                return;
            };
            adds.push((*g, WTriple::new(k.s, k.p, k.o)));
        }

        // The one graph the union default graph covers, or `None` if it covers
        // zero or several. Mirrors `scope_triples`'s `DefaultUnion` arm.
        let union_graph: Option<GraphId> = if cached
            .iter()
            .any(|(scope, _)| matches!(scope, SnapshotScope::DefaultUnion))
        {
            let snap = self.store.snapshot();
            let mut graphs = snap
                .graphs()
                .into_iter()
                .filter(|g| !reserved_graph(&snap, *g));
            match (graphs.next(), graphs.next()) {
                (Some(g), None) => Some(g),
                _ => None,
            }
        } else {
            None
        };

        /// The delta rows that apply to a snapshot over graph `g` alone.
        fn rows_in(rows: &[(GraphId, WTriple)], g: GraphId) -> Vec<WTriple> {
            rows.iter()
                .filter(|(rg, _)| *rg == g)
                .map(|(_, t)| *t)
                .collect()
        }

        // Decide every scope before mutating any of them.
        let mut plans: Vec<(SnapshotScope, Vec<WTriple>, Vec<WTriple>)> =
            Vec::with_capacity(cached.len());
        for (scope, base_rows) in cached {
            let g = match scope {
                SnapshotScope::DefaultStrict => DEFAULT_GRAPH,
                SnapshotScope::DefaultUnion => {
                    let Some(g) = union_graph else {
                        self.invalidate();
                        return;
                    };
                    // A row outside the union's single graph either changes the
                    // graph set or retracts from a graph this snapshot merged —
                    // neither is expressible as a delta on the merged rows.
                    if dels.iter().chain(adds.iter()).any(|(rg, _)| *rg != g) {
                        self.invalidate();
                        return;
                    }
                    g
                }
                // Nothing else is memoisable (`SnapshotScope::memoisable`); if
                // that ever changes, a rebuild stays correct.
                _ => {
                    self.invalidate();
                    return;
                }
            };
            let (d, a) = (rows_in(&dels, g), rows_in(&adds, g));
            if d.len() + a.len() > base_rows / SNAPSHOT_DELTA_REBUILD_DIVISOR {
                self.invalidate();
                return;
            }
            plans.push((scope, d, a));
        }

        let merged = {
            let snapshots = self.snapshots.get_mut().expect("snapshot lock poisoned");
            plans.iter().all(|(scope, d, a)| {
                match snapshots.get_mut(scope).and_then(Arc::get_mut) {
                    Some(src) => {
                        src.apply_delta(d, a);
                        true
                    }
                    // A concurrent reader still holds this snapshot, so it
                    // cannot be mutated in place. Rare; just rebuild.
                    None => false,
                }
            })
        };
        // `all` short-circuits, so an earlier scope may already carry the delta.
        // Each merged snapshot is correct on its own, and `invalidate` drops
        // them all anyway, so a partial merge cannot leave a stale entry.
        if !merged {
            self.invalidate();
        }
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
    /// The seam SPEC-28 phase 3 needs to seed named graphs. Phase 4 (#267)
    /// made the SPARQL `Store` write trait itself quad-shaped
    /// ([`Store::apply_quads`]); wiring `INSERT DATA { GRAPH … }` and GSP
    /// through it (`crate::update`) is a separate task within that phase.
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

    /// Non-interning `QuadKey` lookup: `None` if `g`/`s`/`p`/`o` has never
    /// been interned, meaning the quad cannot be live. Used on the delete
    /// side of [`Store::apply_quads`], mirroring [`Self::intern_key`]'s
    /// interning lookup on the insert side.
    fn lookup_key(
        &self,
        g: GraphId,
        s: &oxrdf::Term,
        p: &oxrdf::Term,
        o: &oxrdf::Term,
    ) -> Option<QuadKey> {
        let d = self.store.dictionary();
        Some(QuadKey::new(g, d.get(s)?, d.get(p)?, d.get(o)?))
    }

    /// Resolve a [`GraphName`](crate::exec::GraphName) for a *deletion*: a
    /// graph nobody has interned yet necessarily holds nothing, so it never
    /// needs interning — `None` means "this quad retracts nothing" (mirrors
    /// `horndb_storage::Store::apply_quads`'s own non-interning term lookup
    /// for `dels`).
    fn resolve_graph_for_delete(&self, graph: Option<&str>) -> Option<GraphId> {
        match graph {
            None => Some(DEFAULT_GRAPH),
            Some(iri) => self.graph_id(iri),
        }
    }

    /// Resolve a [`GraphName`](crate::exec::GraphName) for an *insertion*,
    /// interning a never-seen named graph so the write can create it.
    fn resolve_graph_for_insert(&self, graph: Option<&str>) -> Result<GraphId> {
        match graph {
            None => Ok(DEFAULT_GRAPH),
            Some(iri) => self
                .store
                .intern_graph_uri(&OxTerm::NamedNode(NamedNode::new_unchecked(iri)))
                .map_err(|e| SparqlError::Executor(format!("intern graph: {e}"))),
        }
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
    /// SPEC-28 S3. `GRAPH ?g` has no snapshot form — see [`SnapshotScope`].
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
            ResolvedScope::PerGraph { var, .. } => return Err(per_graph_needs_the_scan_loop(var)),
        })
    }

    /// Whether an empty group pattern (`{}`) matches under `scope`.
    ///
    /// True everywhere except inside a ground `GRAPH <g>` naming a graph the
    /// dataset does not have — see [`ScanScope::ground_graph`] for why the
    /// zero-pattern shortcut has to ask. `resolved` has already applied the
    /// `FROM NAMED` filter and the dictionary lookup, so only a graph that
    /// survives both *and* holds at least one quad reaches `true`.
    fn empty_group_matches(&self, scope: &ScanScope<'_>, resolved: &SnapshotScope) -> bool {
        if scope.ground_graph().is_none() {
            return true;
        }
        match resolved {
            SnapshotScope::OneGraph(g) => self.store.snapshot().graph_len(*g) > 0,
            // An unknown or dataset-excluded graph name resolved to the empty
            // scope; the whole-store scopes are unreachable for a ground
            // `GRAPH <g>`.
            _ => false,
        }
    }

    /// Get-or-build the WCOJ snapshot for `scope`.
    ///
    /// **Only the two whole-store scopes are memoised.** They cost O(store)
    /// to build and every unqualified query wants one, so caching them is
    /// the pre-SPEC-28 behaviour preserved. A graph-scoped build is
    /// O(that graph) — but note `GRAPH ?g` asks for one per graph, so a
    /// query enumerating every graph pays roughly one whole-store build per
    /// execution (six sorted orderings' worth), with no reuse between
    /// executions. That is the accepted price of the bound below, not an
    /// oversight: caching would buy little per graph and cost a cache with
    /// no ceiling: one `Arc<VecTripleSource>` — six sorted index copies —
    /// per graph ever named, evicted only by a write, and reachable from an
    /// unauthenticated `/query` (`EXPLAIN` populates it without executing
    /// anything). A client walking `GRAPH <g1>`…`GRAPH <gN>` would pin ~6×
    /// the store. See `graph_scoped_snapshots_are_not_memoised`.
    ///
    /// A small write merges its delta into the memoised entries in place
    /// ([`Self::apply_delta_to_snapshots`]); every other write drops the memo
    /// wholesale ([`Self::invalidate`]).
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
                // Recomputed on each memo miss. A memo *hit* is safe too:
                // the only write that leaves a `DefaultUnion` entry alive is
                // a delta confined to the one graph the union already covers,
                // so the graph set cannot change underneath it (see
                // `apply_delta_to_snapshots`).
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
    /// snapshot scan) and replaces the cache. Correct across writes because
    /// every write path clears `stats_cache` explicitly (see the field's doc):
    /// a delta merge keeps the snapshot's `Arc` pointer, so `Arc::ptr_eq` on
    /// its own could not tell a stale entry from a fresh one.
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
    /// Resolves `dels` and `adds` against this store's dictionary (dels
    /// non-interning — an unseen graph/term retracts nothing; adds
    /// interning, so a never-seen named graph or term is created), then
    /// applies both in one call to `horndb_storage::Store::apply_quads`
    /// (Task 1, SPEC-28 S6) — the atomic dels-before-adds, idempotent,
    /// counted store boundary. `live_keys` is kept in step (dels removed
    /// before adds inserted, mirroring the storage batch's own ordering) —
    /// not required for correctness (the store is authoritative either way),
    /// but keeps the O(1) fast path in `insert_oxrdf_in_graph` and friends
    /// accurate rather than merely harmless-if-stale.
    fn apply_quads(
        &mut self,
        dels: Vec<AlgebraQuad>,
        adds: Vec<AlgebraQuad>,
    ) -> Result<ApplyCounts> {
        let mut del_rows: Vec<(GraphId, OxTerm, OxTerm, OxTerm)> = Vec::with_capacity(dels.len());
        for (g, s, p, o) in &dels {
            let Some(gid) = self.resolve_graph_for_delete(g.as_deref()) else {
                continue;
            };
            let (Ok(so), Ok(po), Ok(oo)) = (
                algebra_to_oxrdf(s),
                algebra_to_oxrdf(p),
                algebra_to_oxrdf(o),
            ) else {
                continue; // a variable / RDF 1.2 triple term retracts nothing
            };
            del_rows.push((gid, so, po, oo));
        }

        let mut add_rows: Vec<(GraphId, OxTerm, OxTerm, OxTerm)> = Vec::with_capacity(adds.len());
        for (g, s, p, o) in &adds {
            let (Ok(so), Ok(po), Ok(oo)) = (
                algebra_to_oxrdf(s),
                algebra_to_oxrdf(p),
                algebra_to_oxrdf(o),
            ) else {
                continue; // a variable / RDF 1.2 triple term cannot be stored
            };
            let gid = self.resolve_graph_for_insert(g.as_deref())?;
            add_rows.push((gid, so, po, oo));
        }

        let report = self
            .store
            .apply_quads(&del_rows, &add_rows)
            .map_err(|e| SparqlError::Executor(format!("storage apply_quads: {e}")))?;

        for (g, s, p, o) in &del_rows {
            if let Some(key) = self.lookup_key(*g, s, p, o) {
                self.live_keys.remove(&key);
            }
        }
        for (g, s, p, o) in &add_rows {
            // `store.apply_quads` above already interned every add term, so
            // this re-intern is a cheap dictionary hit, never a fresh entry.
            if let Ok(key) = self.intern_key(*g, s, p, o) {
                self.live_keys.insert(key);
            }
        }
        if report.retracted > 0 || report.inserted > 0 {
            self.apply_delta_to_snapshots(&del_rows, &add_rows);
        }
        Ok(ApplyCounts {
            retracted: report.retracted,
            inserted: report.inserted,
        })
    }

    /// Sweeps `graph` through `horndb_storage::Tier::apply_quad_batch`
    /// directly (the id-keyed tier level, not `Store::apply_quads`'s
    /// dictionary-`Term` level — the ids are already in hand from the
    /// snapshot scan, so going one layer down skips a needless decode/
    /// re-intern round trip). A pure deletion batch, so this is still
    /// "via `apply_quads` internally" per the trait doc — `apply_quad_batch`
    /// is the same S6 atomic-batch primitive one layer lower.
    fn clear_graph(&mut self, graph: &spargebra::algebra::GraphTarget) -> Result<usize> {
        use spargebra::algebra::GraphTarget;
        let snap = self.store.snapshot();
        let graphs_to_sweep: Vec<GraphId> = match graph {
            GraphTarget::DefaultGraph => vec![DEFAULT_GRAPH],
            GraphTarget::AllGraphs => snap.graphs(),
            GraphTarget::NamedGraphs => snap
                .graphs()
                .into_iter()
                .filter(|&g| g != DEFAULT_GRAPH)
                .collect(),
            GraphTarget::NamedNode(n) => self.graph_id(n.as_str()).into_iter().collect(),
        };
        let dels: Vec<(GraphId, TermId, TermId, TermId)> = graphs_to_sweep
            .iter()
            .flat_map(|&g| {
                snap.iter_graph_term_ids(g)
                    .map(move |(s, p, o)| (g, s, p, o))
            })
            .collect();
        if dels.is_empty() {
            return Ok(0);
        }
        let report = self
            .store
            .tier()
            .apply_quad_batch(&dels, &[])
            .map_err(|e| SparqlError::Executor(format!("clear_graph: {e}")))?;
        for &(g, s, p, o) in &dels {
            self.live_keys.remove(&QuadKey::new(g, s, p, o));
        }
        if report.retracted > 0 {
            self.invalidate();
        }
        Ok(report.retracted)
    }

    /// SPEC-28 D11: a named graph exists iff it holds at least one visible
    /// quad at this pinned snapshot.
    fn graph_exists(&self, graph: &str) -> bool {
        match self.graph_id(graph) {
            Some(g) => self.store.snapshot().graph_len(g) > 0,
            None => false,
        }
    }

    /// Every named graph (the default-graph sentinel excluded) holding at
    /// least one visible quad, sorted by IRI. Unlike
    /// [`Executor::named_graphs`], this applies no `FROM NAMED`/reserved-
    /// prefix filtering — `DROP ALL`/ADD-MOVE-COPY enumeration is a
    /// store-management question, not a query-dataset one.
    fn graphs(&self) -> Vec<String> {
        let snap = self.store.snapshot();
        let mut out: Vec<String> = snap
            .graphs()
            .into_iter()
            .filter(|&g| g != DEFAULT_GRAPH)
            .filter_map(|g| match snap.graph_uri(g) {
                Ok(OxTerm::NamedNode(n)) => Some(n.into_string()),
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    /// The `ADD`/`MOVE`/`COPY` source read: every visible triple in the one
    /// graph `graph` names. An unknown named graph reads as empty, not an
    /// error (SPEC-28 S3's "unknown graph ⇒ zero rows" rule, applied here to
    /// a read rather than a scan scope).
    fn scan_graph_quads(
        &self,
        graph: &spargebra::algebra::GraphTarget,
    ) -> Result<Vec<AlgebraTriple>> {
        use spargebra::algebra::GraphTarget;
        let gid = match graph {
            GraphTarget::DefaultGraph => DEFAULT_GRAPH,
            GraphTarget::NamedNode(n) => match self.graph_id(n.as_str()) {
                Some(g) => g,
                None => return Ok(Vec::new()),
            },
            GraphTarget::AllGraphs | GraphTarget::NamedGraphs => {
                return Err(SparqlError::Executor(
                    "scan_graph_quads: AllGraphs/NamedGraphs names no single source graph".into(),
                ));
            }
        };
        let triples = self
            .store
            .snapshot()
            .scan_graph(gid)
            .map_err(|e| SparqlError::Executor(format!("scan_graph: {e}")))?;
        Ok(triples
            .iter()
            .map(|(s, p, o)| {
                (
                    oxrdf_to_algebra(s),
                    oxrdf_to_algebra(p),
                    oxrdf_to_algebra(o),
                )
            })
            .collect())
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
        // (parity with MemStore and the SPARQL algebra) — unless the scope is
        // a ground `GRAPH <g>` the dataset does not have, which matches
        // nothing (`ScanScope::ground_graph`).
        if patterns.is_empty() {
            let rows = self
                .empty_group_matches(scope, &resolved)
                .then(Bindings::new);
            return Ok(Box::new(rows.into_iter()));
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

    /// The graphs `GRAPH ?g` enumerates, sorted by IRI.
    ///
    /// Reads the graphs holding visible quads at one pinned snapshot
    /// (SPEC-28 phase 2's visibility-filtered `graphs()`), drops the
    /// default-graph sentinel (D3), then applies the named set. `?g` binds
    /// to `Slot::Id(TermId(g.0))`: a `GraphId` *is* the interned `TermId` of
    /// the graph's IRI (`horndb_storage::store`), so the ordinary
    /// `decode_term` turns it back into the IRI at the result boundary and
    /// it joins an ordinary scan column by raw id.
    ///
    /// Snapshot note: this pins its own store snapshot, and each per-graph
    /// scan the loop then runs pins another (`scope_triples`), so one
    /// `GRAPH ?g` reads N+1 pinned views rather than the single one SPEC-28
    /// S2 describes. They cannot disagree today — every write takes
    /// `&mut self` and no read holds it, so no write can interleave with a
    /// query. Threading one snapshot through would have to widen the whole
    /// `Executor` read seam (`scan_bgp_ids` and friends take no snapshot),
    /// which is out of proportion to a difference that is currently
    /// unobservable; revisit when writes become concurrent with reads.
    fn named_graphs(&self, named: Option<&[String]>) -> Result<Vec<NamedGraph>> {
        let snap = self.store.snapshot();
        let mut out: Vec<NamedGraph> = Vec::new();
        for g in snap.graphs() {
            // `graph_uri` errors on DEFAULT_GRAPH (a sentinel with no IRI),
            // which is also exactly the graph `GRAPH ?g` must never bind.
            let Ok(OxTerm::NamedNode(n)) = snap.graph_uri(g) else {
                continue;
            };
            let iri = n.into_string();
            let admitted = match named {
                // No `FROM NAMED`: every non-reserved graph.
                None => !is_reserved_graph(&iri),
                // `FROM NAMED …`: exactly these, reserved included — naming
                // a reserved graph is the opt-in.
                Some(list) => list.iter().any(|n| n == &iri),
            };
            if admitted {
                out.push(NamedGraph {
                    iri,
                    binding: Slot::Id(TermId(g.0)),
                });
            }
        }
        out.sort_by(|a, b| a.iri.cmp(&b.iri));
        Ok(out)
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
        // See `scan_bgp`: a ground `GRAPH <g>` the dataset does not have
        // matches nothing, not the unit row.
        if patterns.is_empty() {
            return Ok(if self.empty_group_matches(scope, &resolved) {
                Batch::unit()
            } else {
                Batch::empty()
            });
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
        // node shares one snapshot. Every write path clears the cache, so no
        // stale entry can survive a mutation — see the `stats_cache` field.
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
        // fall through to a wider count (SPEC-28 S3). `GRAPH ?g` spans
        // several snapshots — decline, and the caller's scan fallback (the
        // per-graph loop) supplies the count.
        if scope.resolve().is_per_graph() {
            return Ok(None);
        }
        let resolved = self.resolve_scope(scope)?;
        // The empty BGP is the join identity: one solution — zero when the
        // scope is a ground `GRAPH <g>` the dataset does not have.
        if patterns.is_empty() {
            return Ok(Some(usize::from(
                self.empty_group_matches(scope, &resolved),
            )));
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
        // See `count_bgp`: resolve the scope before any counting, and
        // decline `GRAPH ?g` outright.
        if scope.resolve().is_per_graph() {
            return Ok(None);
        }
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
    use spargebra::algebra::GraphTarget;

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

        // Same bound for `GRAPH ?g`, which walks every graph in one scan:
        // it reads each through the same per-graph (uncached) path, so the
        // memo stays at the one whole-store entry.
        let var = Var::new("g");
        let per_graph = GraphScope::Named(GraphSpec::Var(var.clone()));
        let scope = ScanScope::new(&per_graph, &dataset, crate::DefaultGraphMode::Union);
        let batch = crate::exec::op::scan_scoped(&b, &patterns, &scope).unwrap();
        assert_eq!(batch.rows.len(), 20, "one row per graph");
        assert!(
            batch.schema.iter().any(|v| v.name() == var.name()),
            "the scan carries the graph column: {:?}",
            batch.schema
        );
        assert_eq!(
            b.memo_len(),
            1,
            "GRAPH ?g over 20 graphs must not cache 20 snapshots"
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
    fn apply_quads_routes_by_graph() {
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // Same predicate in both graphs, so a scope mixup would be visible.
        let default_add: AlgebraQuad = (None, iri("s1"), iri("p"), iri("o1"));
        let named_add: AlgebraQuad = (Some("http://ex/g".into()), iri("s2"), iri("p"), iri("o2"));
        let report = b
            .apply_quads(Vec::new(), vec![default_add, named_add])
            .unwrap();
        assert_eq!(
            report,
            ApplyCounts {
                retracted: 0,
                inserted: 2
            }
        );
        assert_eq!(b.len(), 2, "both quads are live, in their own graphs");

        assert_eq!(
            b.scan_graph_quads(&GraphTarget::DefaultGraph).unwrap(),
            vec![(iri("s1"), iri("p"), iri("o1"))],
            "the default-graph add must not land in the named graph"
        );
        assert_eq!(
            b.scan_graph_quads(&GraphTarget::NamedNode(NamedNode::new_unchecked(
                "http://ex/g"
            )))
            .unwrap(),
            vec![(iri("s2"), iri("p"), iri("o2"))],
            "the named-graph add must not land in the default graph"
        );
    }

    #[test]
    fn apply_counts_are_accurate() {
        let mut b = HornBackend::new();
        let q = || -> AlgebraQuad {
            (
                None,
                Term::Iri("http://ex/s".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/o".into()),
            )
        };

        // Fresh insert counts.
        assert_eq!(
            b.apply_quads(Vec::new(), vec![q()]).unwrap(),
            ApplyCounts {
                retracted: 0,
                inserted: 1
            }
        );
        // Re-inserting an already-live quad is a counted no-op.
        assert_eq!(
            b.apply_quads(Vec::new(), vec![q()]).unwrap(),
            ApplyCounts {
                retracted: 0,
                inserted: 0
            }
        );
        // Deleting a live quad counts it.
        assert_eq!(
            b.apply_quads(vec![q()], Vec::new()).unwrap(),
            ApplyCounts {
                retracted: 1,
                inserted: 0
            }
        );
        // Deleting an absent quad is a counted no-op.
        assert_eq!(
            b.apply_quads(vec![q()], Vec::new()).unwrap(),
            ApplyCounts {
                retracted: 0,
                inserted: 0
            }
        );

        // Delete + re-add the same quad within one batch: dels apply
        // before adds (S6), so the quad ends present; per Task 1's
        // documented `apply_quad_batch` contract each half is still
        // counted once (retracted AND inserted), even though the net
        // effect on the store is a no-op.
        b.apply_quads(Vec::new(), vec![q()]).unwrap(); // resurrect for the next check
        assert_eq!(
            b.apply_quads(vec![q()], vec![q()]).unwrap(),
            ApplyCounts {
                retracted: 1,
                inserted: 1
            }
        );
        assert_eq!(b.len(), 1, "the quad ends present");
    }

    #[test]
    fn clear_graph_and_exists() {
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        b.apply_quads(
            Vec::new(),
            vec![
                (None, iri("s0"), iri("p"), iri("o0")),
                (Some("http://ex/g1".into()), iri("s1"), iri("p"), iri("o1")),
                (Some("http://ex/g2".into()), iri("s2"), iri("p"), iri("o2")),
            ],
        )
        .unwrap();

        assert!(!b.graph_exists("http://ex/never-seen"));
        assert!(b.graph_exists("http://ex/g1"));
        assert!(b.graph_exists("http://ex/g2"));
        assert_eq!(
            b.graphs(),
            vec!["http://ex/g1".to_owned(), "http://ex/g2".to_owned()]
        );

        let removed = b
            .clear_graph(&GraphTarget::NamedNode(NamedNode::new_unchecked(
                "http://ex/g1",
            )))
            .unwrap();
        assert_eq!(removed, 1);
        assert!(
            !b.graph_exists("http://ex/g1"),
            "SPEC-28 D11: swept to zero quads, so it ceases to exist"
        );
        assert!(b.graph_exists("http://ex/g2"), "g2 is untouched");
        assert_eq!(b.graphs(), vec!["http://ex/g2".to_owned()]);

        let removed = b.clear_graph(&GraphTarget::AllGraphs).unwrap();
        assert_eq!(removed, 2, "default graph's quad + g2's quad");
        assert!(b.is_empty());
        assert!(b.graphs().is_empty());
    }

    #[test]
    fn scan_graph_quads_roundtrip() {
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let g = "http://ex/g";
        b.apply_quads(
            Vec::new(),
            vec![
                (Some(g.to_owned()), iri("s1"), iri("p"), iri("o1")),
                (Some(g.to_owned()), iri("s2"), iri("p"), iri("o2")),
                (None, iri("s3"), iri("p"), iri("o3")), // must not leak into the named graph
            ],
        )
        .unwrap();

        let got: HashSet<AlgebraTriple> = b
            .scan_graph_quads(&GraphTarget::NamedNode(NamedNode::new_unchecked(g)))
            .unwrap()
            .into_iter()
            .collect();
        let want: HashSet<AlgebraTriple> = [
            (iri("s1"), iri("p"), iri("o1")),
            (iri("s2"), iri("p"), iri("o2")),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);

        // An unknown named graph reads as empty, not an error.
        assert_eq!(
            b.scan_graph_quads(&GraphTarget::NamedNode(NamedNode::new_unchecked(
                "http://ex/nope"
            )))
            .unwrap(),
            Vec::new()
        );

        // AllGraphs/NamedGraphs are not a single source — a caller error.
        assert!(b.scan_graph_quads(&GraphTarget::AllGraphs).is_err());
        assert!(b.scan_graph_quads(&GraphTarget::NamedGraphs).is_err());
    }

    #[test]
    fn clear_graph_all_graphs_sweeps_named_graphs() {
        let mut b = HornBackend::new();
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

        let removed = b.clear_graph(&GraphTarget::AllGraphs).unwrap();

        assert_eq!(removed, 2);
        assert!(
            b.is_empty(),
            "clear_graph(AllGraphs) must sweep named graphs too, not just the default graph"
        );
        assert!(
            b.store.snapshot().graphs().is_empty(),
            "SPEC-28 D11: a fully-retracted graph must cease to exist"
        );
        assert!(b.live_keys.is_empty());
    }

    #[test]
    fn clear_graph_all_graphs_sweeps_a_store_with_no_funnel_writes() {
        let mut b = HornBackend::new();
        // Plant a named-graph quad directly at the storage layer, bypassing
        // HornBackend's write funnel entirely, so `live_keys` stays empty on
        // entry. This is the case `clear_graph`'s early-out must not skip:
        // consulting `live_keys.is_empty()` instead of the snapshot scan
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

        let removed = b.clear_graph(&GraphTarget::AllGraphs).unwrap();

        assert_eq!(removed, 1);
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
