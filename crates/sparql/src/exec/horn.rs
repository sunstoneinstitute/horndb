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
use horndb_metrics::labels::ExecPhase;
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
#[derive(Debug, Clone)]
pub struct ReasonStats {
    /// Triples loaded into the backend (asserted base + inferred).
    pub loaded: u64,
    /// Asserted triples in the input dataset's default graph.
    pub asserted: usize,
    /// OWL 2 RL inconsistency witnesses: individuals inferred to be
    /// `owl:Nothing`, in lexical form, capped at
    /// [`INCONSISTENT_WITNESS_CAP`]. Empty iff the closure is consistent.
    pub inconsistent: Vec<String>,
}

/// How many `owl:Nothing` individuals [`load_with_reasoning`] reports. An
/// inconsistent ontology can make every individual an `owl:Nothing`, and the
/// caller logs this list, so it is capped rather than unbounded.
#[cfg(feature = "reasoner")]
pub const INCONSISTENT_WITNESS_CAP: usize = 20;

/// Run the OWL 2 RL `horndb_owlrl` `Engine` over `dataset`'s default graph and
/// load the full materialized closure — asserted base plus everything inferred
/// — into `backend`. `closure` selects which backend closes the transitive- and
/// equivalence-shaped rules; every other rule is compiled rule firing either
/// way. The two closures are differentially gated to the same triple set by
/// `crates/owlrl/tests/closure_backend_differential.rs`.
#[cfg(feature = "reasoner")]
pub fn load_with_reasoning(
    backend: &mut HornBackend,
    dataset: &oxrdf::Dataset,
    closure: horndb_owlrl::BackendChoice,
) -> Result<ReasonStats> {
    let mut engine = horndb_owlrl::integration::Engine::with_backend(closure);
    engine
        .load(dataset)
        .map_err(|e| SparqlError::Executor(format!("owlrl load: {e}")))?;
    let asserted = engine.asserted_len().unwrap_or(0);
    let inconsistent = engine.inconsistent_individuals(INCONSISTENT_WITNESS_CAP);
    // Ids, not strings: the closure crosses this boundary as engine term
    // ids plus the engine's dictionary, so the backend interns once per
    // distinct term instead of decoding, re-parsing and re-interning three
    // strings per closure triple (HDB-117).
    let triples = engine
        .materialized_triple_ids()
        .ok_or_else(|| SparqlError::Executor("owlrl produced no state".into()))?;
    let entries = engine
        .dictionary_entries()
        .ok_or_else(|| SparqlError::Executor("owlrl produced no state".into()))?;
    let loaded = backend.load_id_closure(entries, &triples)?;
    Ok(ReasonStats {
        loaded,
        asserted,
        inconsistent,
    })
}

use crate::algebra::{TriplePattern, Var};
#[cfg(feature = "incremental")]
use crate::exec::circuit;
use crate::exec::scope::{
    graph_var_needs_a_per_graph_node, is_reserved_graph, NamedGraph, ResolvedScope, ScanScope,
};
use crate::exec::store_source::{QuerySource, StoreTripleSource};
use crate::exec::{
    AlgebraQuad, AlgebraTriple, ApplyCounts, Bindings, Executor, GroupCount, Pinnable, Slot, Store,
};
use arrow::array::UInt64Array;
use horndb_metrics::labels::LoadPhase;
use horndb_storage::{
    GraphId, InternedQuad, PinnedSnapshot, Store as ColumnStore, StoreSnapshot, TermId,
    DEFAULT_GRAPH,
};
use horndb_wcoj::estimator::StatsEstimator;
use horndb_wcoj::executor::Executor as WcojExecutor;
use horndb_wcoj::ids::Triple as WTriple;
use horndb_wcoj::pattern::{Bgp as WBgp, Term as WTerm, TriplePattern as WPattern, Var as WVar};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::source::TripleSource;
use horndb_wcoj::stats::{SnapshotStats, Stats, ZeroStats};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// Cheap size stats for scrape-time metrics (see [`HornBackend::storage_stats`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct HornStorageStats {
    pub triples: u64,
    pub graphs: u64,
    pub predicates: u64,
    pub dictionary_terms: u64,
    pub dictionary_terms_live: u64,
    /// Total estimated bytes across every tier (warm + cold).
    pub bytes_estimated: u64,
    /// The part of `bytes_estimated` held by cold, memory-mapped partitions
    /// (SPEC-25 S5). Warm bytes are `bytes_estimated - bytes_cold`.
    pub bytes_cold: u64,
    /// Approximate heap bytes the term dictionary owns (HDB-146). O(1) to
    /// read — see `horndb_storage::Dictionary::approx_bytes`.
    pub dictionary_bytes: u64,
}

/// Where a serving process's heap actually goes (HDB-146), for the components
/// that can account for themselves. Everything else — allocator retention,
/// per-query intermediates, the binary and its stacks — is the residual
/// against RSS, which is the number the footprint measurement reports.
///
/// The memoised direct source (`StoreTripleSource`) is deliberately absent:
/// its leaves may be `Arc`-clones of the partitions' own columns, so counting
/// them would double-count `partitions`. It is zero on the default read path.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemorySplit {
    /// Columnar partitions, as the tier estimates them.
    pub partitions: u64,
    pub dictionary_keys: u64,
    pub dictionary_terms: u64,
    pub dictionary_index: u64,
    /// Every memoised `VecTripleSource`, summed over scopes.
    pub snapshots: u64,
    /// Every cached planner summary, summed over scopes.
    pub stats: u64,
}

impl MemorySplit {
    pub fn total(&self) -> u64 {
        self.partitions
            + self.dictionary_keys
            + self.dictionary_terms
            + self.dictionary_index
            + self.snapshots
            + self.stats
    }

    /// `(label, bytes)` in report order.
    pub fn rows(&self) -> [(&'static str, u64); 6] {
        [
            ("partitions", self.partitions),
            ("dict keys", self.dictionary_keys),
            ("dict terms", self.dictionary_terms),
            ("dict index", self.dictionary_index),
            ("query snapshots", self.snapshots),
            ("planner stats", self.stats),
        ]
    }
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
/// `GRAPH ?g` has no variant here on purpose: it binds a per-row graph
/// column rather than reading one flattened source (SPEC-28 D6). The
/// `PerGraph` operator substitutes the graph it is currently on before a
/// leaf's scope is resolved, so [`HornBackend::resolve_scope`] only ever
/// sees `OneGraph` here — and refuses an unsubstituted graph variable if
/// one somehow reaches it.
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

    /// The other whole-store scope, when a store shaped like
    /// [`HornBackend::default_scopes_coincide`] makes it read the exact same
    /// triples as this one. `None` for every other scope.
    fn default_twin(&self) -> Option<SnapshotScope> {
        match self {
            SnapshotScope::DefaultStrict => Some(SnapshotScope::DefaultUnion),
            SnapshotScope::DefaultUnion => Some(SnapshotScope::DefaultStrict),
            _ => None,
        }
    }
}

/// The empty graph: zero rows, no error. (Distinct from `scope.rs`'s
/// `EMPTY_GRAPH_SET`, which is the *name* list this resolves from.)
const EMPTY_GRAPH_SCOPE: SnapshotScope = SnapshotScope::FromUnion(Vec::new());

/// Whether reads go through the direct partition source (HDB-120).
///
/// **Off by default — opt in with `HORNDB_DIRECT_SOURCE=1`** (or `on`/`true`).
///
/// The direct source is correct (`crates/sparql/tests/direct_source_parity.rs`
/// checks it against the `VecTripleSource` oracle) and cuts the serving
/// footprint by dropping the per-query copy, but it is **not faster**: the
/// hornbench A/B (HDB-144, `docs/benchmarks.md`) puts warm reads **1.16-6.14x
/// slower** on trainmarks xlarge and LDBC SPB-256 `aggregation-qps`
/// **4.13x slower** (56.60 -> 13.71), for an **8.2%** RSS saving. Neither gate
/// is met, so the default stays off. (An earlier laptop smoke on trainmarks
/// medium predicted 2-8x; hornbench confirms the shape.)
/// The gap is the merged cursor's inner loop, not source construction —
/// `MergedIter::peek`/`seek` walk a live-leaf list at every step where
/// `VecIter` indexes one flat column, and `MergedIter` has no `active_run`, so
/// the k==2 SIMD-intersect fast path in `executor/wcoj.rs::BatchIter` never
/// arms. Closing that (a single-live-leaf specialization plus `active_run`) is
/// the follow-up; the default flips when a hornbench A/B says it should.
/// Read once per process and cached.
fn direct_source_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("HORNDB_DIRECT_SOURCE").as_deref(),
            Ok("1") | Ok("on") | Ok("true")
        )
    })
}

/// Whether every coarse write funnel demotes the whole store to the cold tier
/// when it finishes (SPEC-25 S5 acceptance #5).
///
/// **Off by default — opt in with `HORNDB_COLD_TIER=1`** (or `on`/`true`).
///
/// A boolean, not a directory: the store owns its cold directory
/// (`horndb_storage::Store::cold_dir` — `<dir>/cold` for a durable store, a
/// process-unique temp dir for an in-memory one), so there is nothing to
/// inject. When set, [`HornBackend::demote_all_if_cold_tier`] runs at the end
/// of each write funnel, so every subsequent query reads a fully cold store
/// and every write promotes, writes, and re-demotes. That is the strictest
/// "mixed warm/cold" configuration, and it is what the conformance harness
/// runs in CI to prove the tier is transparent.
///
/// It is a test knob, not a placement policy: demoting everything after every
/// write is far more aggressive than any real schedule would be.
/// Read once per process and cached.
fn cold_tier_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("HORNDB_COLD_TIER").as_deref(),
            Ok("1") | Ok("on") | Ok("true")
        )
    })
}

/// A delta touching more than `1 / SNAPSHOT_DELTA_REBUILD_DIVISOR` of a cached
/// snapshot's rows is not worth merging in place: at that size a full rebuild
/// from the store costs about the same and is simpler, so the memo is dropped
/// instead. See [`HornBackend::apply_delta_to_snapshots`].
const SNAPSHOT_DELTA_REBUILD_DIVISOR: usize = 2;

/// Most scopes [`HornBackend::snapshot_stats`] keeps a summary for at once.
/// Small: the whole-store scopes plus a few `GRAPH <g>` ones is the shape that
/// benefits; beyond that the cache is being swept, not reused.
const STATS_CACHE_MAX_SCOPES: usize = 8;

/// Most deltas a `StatsSlot::Building` entry queues for replay. Replay runs
/// under the stats-cache lock at O(delta × store) each; past this the slot is
/// dropped and the next query pays one ordinary rebuild instead.
const STATS_PENDING_CAP: usize = 32;

/// True if `g` must stay OUT of the no-dataset default graph (SPEC-27 F6 /
/// SPEC-29 D4/D6): it is a HornDB-internal graph that `visible` has not
/// opted back in. The default-graph sentinel has no IRI (`graph_uri` errors
/// on it) and is never reserved, so it stays in the union default graph.
///
/// `visible` is empty unless `reasoning.default_dataset_includes_inferred`
/// is set, in which case it holds exactly the per-view inferred graphs plus
/// the spine-closure graph — never the view catalog (SPEC-29 D6's "and
/// nothing else").
fn hidden_reserved(snap: &StoreSnapshot<'_>, g: GraphId, visible: &BTreeSet<String>) -> bool {
    match snap.graph_uri(g) {
        Ok(OxTerm::NamedNode(n)) => is_reserved_graph(n.as_str()) && !visible.contains(n.as_str()),
        _ => false,
    }
}

/// One term in the `Engine::materialized_triples()` lexical convention — the
/// exact inverse of [`lexical_to_oxrdf`]. `None` for an RDF 1.2 triple term,
/// which the Stage-1 OWL 2 RL engine does not accept.
pub(crate) fn oxrdf_to_lexical(t: &OxTerm) -> Option<String> {
    match t {
        OxTerm::NamedNode(n) => Some(n.as_str().to_owned()),
        OxTerm::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        OxTerm::Literal(l) => Some(match l.language() {
            Some(lang) => format!("\"{}\"@{lang}", l.value()),
            None => format!("\"{}\"^^<{}>", l.value(), l.datatype().as_str()),
        }),
        #[allow(unreachable_patterns)]
        _ => None,
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
/// Merge one bulk-load phase into the shared counters (SPEC-17 §5.4.1). Called
/// once per phase per batch, never from inside a loop.
fn record_load_phase(phase: LoadPhase, elapsed: std::time::Duration, rows: u64) {
    horndb_metrics::metrics()
        .storage
        .record_load_phase(phase, elapsed, rows);
}

/// The memoised WCOJ sources, tagged with the commit version they were built
/// at (HDB-119). The tag is what makes the memo safe to share between the
/// writable backend and pinned read views running without the store lock: an
/// entry is only ever reused by a reader pinned at the *same* version, so a
/// snapshot built from an older store can never answer a newer query (or the
/// other way round). Untagged, a reader that started its build before a
/// commit could insert a stale entry after it and every later query would
/// read it.
#[derive(Default)]
struct SnapshotMemo {
    /// Commit version every entry in `map` was built at.
    version: u64,
    map: HashMap<SnapshotScope, Arc<VecTripleSource>>,
}

pub struct HornBackend {
    /// Shared with every pinned read view ([`Self::pin_read`]). The storage
    /// crate takes `&self` for writes and serializes them itself, so sharing
    /// the handle does not share write access.
    store: Arc<ColumnStore>,
    /// Lazily-built WCOJ sources, one per [`SnapshotScope`] a query has
    /// asked for. A small `apply_quads` delta is merged into these in place
    /// ([`Self::apply_delta_to_snapshots`]); every other write clears them
    /// wholesale ([`Self::invalidate`]). Most workloads use one entry (the
    /// unqualified default graph); a query mixing `GRAPH` scopes adds one
    /// per distinct scope.
    snapshots: Arc<Mutex<SnapshotMemo>>,
    /// Cached statistics summaries for `EXPLAIN`'s `cardinality_estimate`, one
    /// per [`SnapshotScope`] asked for, each tagged with the store commit
    /// version it describes (HDB-123). Shared with every pinned read view, the
    /// same way the snapshot memo is.
    ///
    /// **The version tag is the whole freshness argument.** An entry is reused
    /// only by a read at the same version, so it cannot answer for a store
    /// that has moved on. That is what lets a `GRAPH <g>` scope be cached at
    /// all: its snapshot is rebuilt per query (`SnapshotScope::memoisable` is
    /// false for it), so there is no stable `Arc` to key on, but the rows it
    /// would rebuild are fixed by the version.
    ///
    /// A write does not drop the whole-store entries. It merges the same quad
    /// delta into them ([`SnapshotStats::apply_delta`]) and re-tags them, so a
    /// small write followed by a read costs no full rebuild — see
    /// [`Self::apply_delta_to_snapshots`]. Entries this cannot maintain (a
    /// graph scope, or one whose drift bound is spent) are dropped instead.
    ///
    /// Holds no `Arc<VecTripleSource>` of its own; only a background build
    /// keeps one, and the delta merge copies on write in that case
    /// (`apply_delta_to_snapshots`).
    stats_cache: Arc<Mutex<HashMap<SnapshotScope, StatsCacheEntry>>>,
    /// `Some` on a pinned read view: every read resolves at that commit
    /// version instead of the store's latest. `None` on the writable backend
    /// (the one the server keeps under its `RwLock`), which always reads the
    /// newest committed state. See [`Self::pin_read`].
    pin: Option<PinnedSnapshot>,
    /// Whether reads take the direct partition source. Defaults to
    /// [`direct_source_enabled`]; [`Self::set_direct_source`] overrides it.
    direct_source: bool,

    /// Whether every coarse write funnel demotes the whole store to the cold
    /// tier when it finishes. Defaults to [`cold_tier_enabled`];
    /// [`Self::set_cold_tier`] overrides it, which is how the parity tests
    /// exercise the funnel wiring without depending on a process-wide env
    /// var.
    cold_tier: bool,

    /// The last [`StoreTripleSource`] handed to a query, with the tier version
    /// it was opened at. Shared with every pinned read view ([`Self::pin_read`]),
    /// the same way the snapshot memo is — a view is built per query, so a
    /// per-view cache would never hit.
    ///
    /// Building one is not free: `PredicatePartition::ordered_at` can only
    /// `Arc`-clone the stored columns while the partition has no retractions
    /// and the read version is at or above its max begin stamp — otherwise it
    /// materializes the visible subset, per predicate, per call. Without this
    /// memo a store that has ever served a `DELETE` pays that copy on every
    /// query, which measured 3-8x slower than the `VecTripleSource` path on
    /// trainmarks (whose q6 deletes before q1..q5 run).
    ///
    /// One entry, not a map: a query stream almost always reads the same
    /// graph, and holding one source per graph would put the footprint this
    /// task exists to cut back on the heap. Keyed by the tier version the
    /// source was opened at, so a write invalidates it with no help from
    /// [`Self::invalidate`] or [`Self::apply_delta_to_snapshots`].
    direct_cache: Arc<Mutex<Option<DirectCacheEntry>>>,
    /// SPEC-29 D7 routing: every graph this backend has actually mutated
    /// since [`Self::take_touched_graphs`] last drained the set. Recorded in
    /// the two write funnels ([`Store::apply_quads`] and
    /// [`Store::clear_graph`]) rather than in each caller, so no write path
    /// can forget to report; a no-op batch (SPEC-28 S6 idempotence) records
    /// nothing, which is what makes a replayed change-feed batch derive
    /// nothing. Empty and untouched when reasoning is off.
    touched_graphs: BTreeSet<GraphId>,
    /// SPEC-29 D6: reserved-namespace graphs nonetheless admitted to the
    /// no-dataset default union and to `GRAPH ?g` enumeration. See
    /// [`hidden_reserved`] and [`Self::set_visible_inferred`].
    visible_inferred: BTreeSet<String>,
    /// SPEC-24 S4: the DBSP circuit this backend's default-graph writes feed,
    /// once [`Self::attach_circuit`] has run. `None` on a fresh backend and on
    /// every pinned read view (a view never writes). See [`circuit`].
    #[cfg(feature = "incremental")]
    circuit: Option<circuit::Wiring>,
}

/// One cached statistics summary: the store commit version it describes, and
/// the summary itself or the build in flight for it (see `planning_stats`).
/// Lines up with HDB-119's version-tagged snapshot memo.
type StatsCacheEntry = (u64, StatsSlot);

/// One scope's planner summary. `Building` marks a build in flight on the
/// `horndb-stats` thread: writes that merge into the snapshot meanwhile queue
/// their deltas in `pending`, and the builder replays them onto the finished
/// summary before it lands (`land_stats`). `id` lets a builder whose slot was
/// dropped and re-created discard its result.
enum StatsSlot {
    Building {
        id: u64,
        pending: Vec<(Vec<WTriple>, Vec<WTriple>)>,
    },
    Ready(Arc<SnapshotStats>),
}

/// Install a finished summary for `key` unless its `Building` slot is gone
/// or belongs to a newer build. Deltas that merged while the build ran are
/// replayed first, against the builder's own pre-merge snapshot; a summary
/// that refuses one (drift spent) is dropped, and the next query rebuilds.
fn land_stats(
    cache: &mut HashMap<SnapshotScope, StatsCacheEntry>,
    key: &SnapshotScope,
    id: u64,
    mut snapshot: Arc<VecTripleSource>,
    mut stats: Arc<SnapshotStats>,
) {
    let pending = match cache.get_mut(key) {
        Some((
            _,
            StatsSlot::Building {
                id: slot_id,
                pending,
            },
        )) if *slot_id == id => std::mem::take(pending),
        _ => return,
    };
    for (d, a) in &pending {
        if !Arc::make_mut(&mut stats).apply_delta(&snapshot, d, a) {
            cache.remove(key);
            return;
        }
        Arc::make_mut(&mut snapshot).apply_delta(d, a);
    }
    if let Some((_, slot)) = cache.get_mut(key) {
        *slot = StatsSlot::Ready(stats);
    }
}

/// The one memoised direct source: the tier version it was opened at, the
/// graph it reads, and the source itself. Version-tagged the same way
/// [`StatsCacheEntry`] is — see `HornBackend::direct_cache`.
type DirectCacheEntry = (u64, GraphId, Arc<StoreTripleSource>);

/// One full snapshot scan into a [`SnapshotStats`], counted and timed as
/// `horndb_sparql_stats_rebuild`.
fn build_stats(snapshot: &VecTripleSource) -> Arc<SnapshotStats> {
    let started = std::time::Instant::now();
    let stats = Arc::new(SnapshotStats::from_source(snapshot));
    let m = &horndb_metrics::metrics().sparql;
    m.stats_rebuild.inc();
    m.stats_rebuild_seconds
        .observe(started.elapsed().as_secs_f64());
    stats
}

impl Default for HornBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HornBackend {
    pub fn new() -> Self {
        Self::with_store(ColumnStore::in_memory())
    }

    /// A backend over a caller-built store — `Store::open(dir)` for one that
    /// survives a restart (SPEC-25 S3), `Store::in_memory()` for [`Self::new`].
    pub fn with_store(store: ColumnStore) -> Self {
        Self {
            store: Arc::new(store),
            snapshots: Arc::new(Mutex::new(SnapshotMemo::default())),
            stats_cache: Arc::new(Mutex::new(HashMap::new())),
            pin: None,
            direct_source: direct_source_enabled(),
            cold_tier: cold_tier_enabled(),
            direct_cache: Arc::new(Mutex::new(None)),
            touched_graphs: BTreeSet::new(),
            visible_inferred: BTreeSet::new(),
            #[cfg(feature = "incremental")]
            circuit: None,
        }
    }

    /// Pin an owned read view at the store's current commit version
    /// (HDB-119). O(1): an `Arc` clone of the store handle plus one tier pin.
    ///
    /// The view shares the store and the snapshot memo, but reads at its
    /// pinned version, so it stays valid — and isolated from concurrent
    /// writers — after the caller drops whatever lock it took this under.
    /// That is what lets the HTTP `/query` handler stream a result with no
    /// lock held. The pin also holds compaction back at its version
    /// (`MemoryTier::compact`), which is what keeps the rows it can still see
    /// from being reclaimed.
    ///
    /// **Read-only.** A view is a `Store` too (the trait is implemented on
    /// the type, not on the mode), but writing through one would bypass the
    /// caller's write serialization; the write methods `debug_assert` against
    /// it.
    pub fn pin_read(&self) -> HornBackend {
        HornBackend {
            store: Arc::clone(&self.store),
            snapshots: Arc::clone(&self.snapshots),
            stats_cache: Arc::clone(&self.stats_cache),
            pin: Some(self.store.pin()),
            direct_source: self.direct_source,
            cold_tier: self.cold_tier,
            direct_cache: Arc::clone(&self.direct_cache),
            // Write-only routing state; a read view never records writes.
            touched_graphs: BTreeSet::new(),
            // Read-side: the view must hide/admit the same graphs as its parent.
            visible_inferred: self.visible_inferred.clone(),
            #[cfg(feature = "incremental")]
            circuit: None,
        }
    }

    /// SPEC-24 S4 (HDB-51): put `circuit` behind this backend's write funnel.
    /// From here on every `Store::apply_quads` batch lowers its **default
    /// graph** net changes to `assert_triple` / `retract_triple` plus one
    /// `tick()`, and the tick's derived delta is mirrored into
    /// [`circuit::DERIVED_GRAPH`], which this call admits to the default union
    /// so a following query sees derived rows. Register rules (`add_plan`,
    /// `add_closure_plan`) on `circuit` first: rule ids are the caller's,
    /// predicate ids come from [`Self::intern_term`]. Rows already in the
    /// default graph (a bulk load, which bypasses the funnel) are asserted
    /// into the circuit here, in one tick.
    ///
    /// Not reachable from `serve` yet: E4 (owlrl Z-set rules) registers the
    /// real rule set; this is the seam it plugs into.
    #[cfg(feature = "incremental")]
    pub fn attach_circuit(&mut self, circuit: horndb_incremental::Circuit) -> Result<()> {
        self.attach_circuit_with_feed_capacity(circuit, circuit::FEED_CAPACITY)
    }

    /// [`Self::attach_circuit`] with an explicit change-feed capacity. Public
    /// so a test can force the `DisconnectSlow` resync path with a handful
    /// of rows instead of `FEED_CAPACITY` of them.
    #[cfg(feature = "incremental")]
    pub fn attach_circuit_with_feed_capacity(
        &mut self,
        circuit: horndb_incremental::Circuit,
        capacity: usize,
    ) -> Result<()> {
        debug_assert!(self.pin.is_none(), "attach_circuit on a pinned read view");
        let graph = self
            .store
            .intern_graph_uri(&OxTerm::NamedNode(NamedNode::new_unchecked(
                circuit::DERIVED_GRAPH,
            )))
            .map_err(|e| SparqlError::Executor(format!("intern derived graph: {e}")))?;
        let mut wiring = circuit::Wiring::new(circuit, Arc::clone(&self.store), graph, capacity);
        let base: Vec<horndb_incremental::TripleId> = self
            .store
            .snapshot()
            .iter_graph_term_ids(DEFAULT_GRAPH)
            .map(|(s, p, o)| (s.0, p.0, o.0))
            .collect();
        wiring.seed(&self.store, &base);
        self.circuit = Some(wiring);
        self.visible_inferred
            .insert(circuit::DERIVED_GRAPH.to_owned());
        self.invalidate();
        Ok(())
    }

    /// The attached circuit, for registering rules or reading its state.
    #[cfg(feature = "incremental")]
    pub fn circuit(&mut self) -> Option<&mut horndb_incremental::Circuit> {
        self.circuit.as_mut().map(circuit::Wiring::circuit)
    }

    /// Times the engine's feed subscription was dropped (`DisconnectSlow`)
    /// and the derived graph rebuilt from the circuit. Zero without a circuit.
    #[cfg(feature = "incremental")]
    pub fn circuit_resyncs(&self) -> u64 {
        self.circuit.as_ref().map_or(0, |w| w.resyncs)
    }

    /// Intern `term` in this store's dictionary and return its id — the
    /// predicate id a circuit rule is registered against.
    #[cfg(feature = "incremental")]
    pub fn intern_term(&self, term: &OxTerm) -> Result<u64> {
        self.store
            .dictionary()
            .intern(term)
            .map(|id| id.0)
            .map_err(|e| SparqlError::Executor(format!("intern: {e}")))
    }

    /// The default-graph triples `apply_quads`'s batch will flip, as the
    /// circuit's `(asserts, retracts)`: present-after minus present-before,
    /// per triple, so a re-insert of a live row or a delete of an absent one
    /// (both no-ops in storage, SPEC-28 S6) reaches the circuit as nothing,
    /// and a delete+insert of one row within a batch nets to nothing. Runs
    /// against the pre-batch snapshot.
    #[cfg(feature = "incremental")]
    fn default_graph_flips(
        &self,
        del_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
        add_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
    ) -> Result<(
        Vec<horndb_incremental::TripleId>,
        Vec<horndb_incremental::TripleId>,
    )> {
        use std::collections::BTreeMap;
        // triple -> (in dels, in adds)
        let mut rows: BTreeMap<horndb_incremental::TripleId, (bool, bool)> = BTreeMap::new();
        for (g, s, p, o) in del_rows {
            if *g != DEFAULT_GRAPH {
                continue;
            }
            if let Some(k) = self.lookup_key(*g, s, p, o) {
                rows.entry((k.s, k.p, k.o)).or_default().0 = true;
            }
        }
        for (g, s, p, o) in add_rows {
            if *g != DEFAULT_GRAPH {
                continue;
            }
            let k = self.intern_key(*g, s, p, o)?;
            rows.entry((k.s, k.p, k.o)).or_default().1 = true;
        }
        let snap = self.store.snapshot();
        let mut asserts = Vec::new();
        let mut retracts = Vec::new();
        for (t, (del, add)) in rows {
            let before = snap.contains_quad(DEFAULT_GRAPH, TermId(t.0), TermId(t.1), TermId(t.2));
            let after = (before && !del) || add;
            match (before, after) {
                (false, true) => asserts.push(t),
                (true, false) => retracts.push(t),
                _ => {}
            }
        }
        Ok((asserts, retracts))
    }

    /// The store view every *read* resolves against: the pinned commit
    /// version on a read view, the latest committed state on the writable
    /// backend. Writes keep reading the latest state directly — a write is
    /// never served from a pinned view.
    fn snap(&self) -> StoreSnapshot<'_> {
        match &self.pin {
            Some(pin) => self.store.snapshot_at(pin),
            None => self.store.snapshot(),
        }
    }

    /// The commit version [`Self::snap`] reads at — the memo tag.
    fn read_version(&self) -> u64 {
        match &self.pin {
            Some(pin) => pin.version(),
            None => self.store.snapshot().version(),
        }
    }

    /// Turn the direct partition source on or off for this backend (HDB-120).
    ///
    /// The default comes from `HORNDB_DIRECT_SOURCE`. This is the in-process
    /// form of the same switch: the A/B driver and the parity test both need
    /// two backends that differ only here.
    pub fn set_direct_source(&mut self, on: bool) {
        self.direct_source = on;
    }

    /// Per-instance form of the `HORNDB_COLD_TIER` knob, mirroring
    /// [`Self::set_direct_source`]. The cold-parity tests use it to prove the
    /// write funnels really demote, which a process-wide env var read once
    /// into a `OnceLock` cannot pin.
    pub fn set_cold_tier(&mut self, on: bool) {
        self.cold_tier = on;
    }

    /// Demote every settled partition of this backend's store to the cold,
    /// memory-mapped tier (SPEC-25 S5). Reads stay correct — the cold form
    /// sits behind the same warm read surface — and the next write to a cold
    /// partition promotes it back first.
    ///
    /// Explicit placement, for tests and for the `HORNDB_COLD_TIER` gate; no
    /// policy calls it on its own yet.
    pub fn demote_all(&self) -> Result<usize> {
        self.store
            .demote_all()
            .map_err(|e| SparqlError::Executor(format!("demote_all: {e}")))
    }

    /// [`Self::demote_all`] when [`cold_tier_enabled`] says so, otherwise
    /// nothing. Every coarse write funnel here calls it; the conformance
    /// harness calls it too, after a per-triple file load that has no funnel
    /// of its own. Deliberately NOT called per triple: each demote encodes a
    /// whole partition, so a per-triple hook would make a bulk load
    /// quadratic.
    pub fn demote_all_if_cold_tier(&self) {
        if self.cold_tier {
            // Not `let _ =`: this knob exists to make the conformance run
            // grade a cold store, so a demote that silently failed would
            // leave the run warm and report a green that proves nothing.
            self.demote_all()
                .expect("HORNDB_COLD_TIER demote_all failed");
        }
    }

    /// Drain the set of graphs mutated since the last call, as SPARQL graph
    /// names (`None` = the default-graph sentinel). SPEC-29 D7's routing
    /// input: the view manager marks the touched graphs' views dirty, bumps
    /// the spine version if a spine graph is among them, and ignores writes
    /// to the reserved namespace because those are its own.
    ///
    /// Only the two write funnels record here, so bulk loaders that write
    /// below them (`insert_oxrdf_batch`, `load_lexical_triples`) report
    /// nothing — the view manager re-scans `graphs()` each pass and picks up
    /// new source graphs that way.
    pub fn take_touched_graphs(&mut self) -> Vec<crate::exec::GraphName> {
        let snap = self.store.snapshot();
        let out = std::mem::take(&mut self.touched_graphs)
            .into_iter()
            .map(|g| match snap.graph_uri(g) {
                Ok(OxTerm::NamedNode(n)) => Some(n.into_string()),
                _ => None,
            })
            .collect();
        drop(snap);
        out
    }

    /// SPEC-29 D6: admit these reserved-namespace graphs to the no-dataset
    /// default union and to `GRAPH ?g` enumeration. Pass exactly the per-view
    /// inferred graphs plus the spine-closure graph; pass an empty set to
    /// hide them again (the default, and what
    /// `reasoning.default_dataset_includes_inferred = false` leaves in place).
    ///
    /// Invalidates the snapshot memo: the `DefaultUnion` scope's graph set
    /// changes underneath it.
    pub fn set_visible_inferred(&mut self, iris: BTreeSet<String>) {
        if self.visible_inferred != iris {
            self.visible_inferred = iris;
            self.invalidate();
        }
    }

    /// Every visible triple in `graph` (`None` = the default graph), in the
    /// `Engine::materialized_triples()` lexical convention — the read side of
    /// a reasoning view's source scan. Graph-scoped (`scan_graph`), never a
    /// whole-store filter. An unknown graph IRI reads as empty, matching
    /// SPEC-28's "unknown graph ⇒ zero rows".
    pub fn scan_graph_lexical(
        &self,
        graph: crate::exec::GraphName,
    ) -> Result<Vec<(String, String, String)>> {
        let gid = match graph.as_deref() {
            None => DEFAULT_GRAPH,
            Some(iri) => match self.graph_id(iri) {
                Some(g) => g,
                None => return Ok(Vec::new()),
            },
        };
        let snap = self.store.snapshot();
        let rows = snap
            .scan_graph(gid)
            .map_err(|e| SparqlError::Executor(format!("scan_graph: {e}")))?;
        Ok(rows
            .iter()
            .filter_map(|(s, p, o)| {
                Some((
                    oxrdf_to_lexical(s)?,
                    oxrdf_to_lexical(p)?,
                    oxrdf_to_lexical(o)?,
                ))
            })
            .collect())
    }

    /// Live triple count across every graph, visibility-filtered (SPEC-25 S1).
    /// (SPEC-28 phase 3 revisits the union default graph.)
    pub fn len(&self) -> u64 {
        self.store.triple_count()
    }

    /// Cheap point-in-time size stats for scrape-time metrics: live triple
    /// count plus the tier's already-tracked graph/predicate/byte estimates and
    /// the dictionary term count. Bounded by the number of distinct
    /// predicates/graphs, with one exception: the first read after a batched
    /// write merges each touched partition's runs (HDB-84), which is O(rows in
    /// that partition) and happens once. `tier.triples` is visibility-filtered
    /// by the tier itself, so no adjustment is needed here.
    pub fn storage_stats(&self) -> HornStorageStats {
        let tier = self.store.stats();
        HornStorageStats {
            triples: tier.triples,
            graphs: tier.graphs,
            predicates: tier.predicates,
            dictionary_terms: self.store.dictionary().len() as u64,
            dictionary_terms_live: self.store.dictionary().live_len() as u64,
            bytes_estimated: tier.bytes_estimated,
            bytes_cold: tier.bytes_cold,
            dictionary_bytes: self.store.dictionary().approx_bytes().total(),
        }
    }

    /// Attribute this backend's heap across the components that can account
    /// for themselves (HDB-146). See [`MemorySplit`] for what is left out.
    pub fn memory_split(&self) -> MemorySplit {
        let dict = self.store.dictionary().approx_bytes();
        let snapshots = self
            .snapshots
            .lock()
            .expect("snapshot lock poisoned")
            .map
            .values()
            .map(|s| s.approx_bytes())
            .sum();
        let stats = self
            .stats_cache
            .lock()
            .expect("stats lock poisoned")
            .values()
            .map(|(_, slot)| match slot {
                StatsSlot::Ready(s) => s.approx_bytes(),
                StatsSlot::Building { .. } => 0,
            })
            .sum();
        MemorySplit {
            partitions: self.store.stats().bytes_estimated,
            dictionary_keys: dict.keys,
            dictionary_terms: dict.terms,
            dictionary_index: dict.index,
            snapshots,
            stats,
        }
    }

    /// Whole-store emptiness — see [`Self::len`] for scope.
    pub fn is_empty(&self) -> bool {
        self.store.triple_count() == 0
    }

    /// A fresh tag for one document load into this backend's store
    /// (HDB-113) — see `horndb_storage::Store::next_bnode_doc_tag`.
    pub fn next_bnode_doc_tag(&self) -> u64 {
        self.store.next_bnode_doc_tag()
    }

    fn invalidate(&mut self) {
        let version = self.store.snapshot().version();
        let mut memo = self.snapshots.lock().expect("snapshot lock poisoned");
        memo.map.clear();
        memo.version = version;
        drop(memo);
        // The snapshots these described are gone; nothing here can be salvaged.
        self.stats_cache
            .lock()
            .expect("stats_cache lock poisoned")
            .clear();
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
    /// 2. The merge is copy-on-write: when a running query or the stats
    ///    builder still holds the snapshot, `Arc::make_mut` clones it first.
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
    ///
    /// 5. The memo still carries `base`, the commit version this write started
    ///    from. A memo tagged anything else was built by a pinned reader
    ///    running without the store lock (HDB-119): older ⇒ stale, drop it;
    ///    newer ⇒ it already includes this write, leave it alone.
    fn apply_delta_to_snapshots(
        &mut self,
        base: u64,
        del_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
        add_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
    ) {
        // A graph-scoped summary has no memoised source to merge a delta
        // against, so a write simply drops it. The whole-store scopes are
        // maintained in place alongside their snapshot, in the merge loop
        // below. Unlike the old snapshot-`Arc`-keyed cache, an entry here is
        // no second strong reference, so keeping it cannot fail the
        // `Arc::get_mut` that merge needs.
        self.stats_cache
            .lock()
            .expect("stats_cache lock poisoned")
            .retain(|scope, _| scope.memoisable());

        // Each memoised scope with the row count it currently holds — only if
        // the memo is the one this write started from (condition 5).
        let cached: Vec<(SnapshotScope, usize)> = {
            let guard = self.snapshots.lock().expect("snapshot lock poisoned");
            if guard.version > base {
                return; // built after this commit: already up to date
            }
            if guard.version < base {
                drop(guard);
                self.invalidate();
                return;
            }
            guard
                .map
                .iter()
                .map(|(scope, src)| (scope.clone(), src.total_triples()))
                .collect()
        };
        if cached.is_empty() {
            // Nothing to merge, but the empty memo must still carry the new
            // version so the next reader's tag comparison is against reality.
            self.invalidate();
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
                .filter(|g| !hidden_reserved(&snap, *g, &self.visible_inferred));
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

        let post = self.store.snapshot().version();
        let merged = {
            let mut memo = self.snapshots.lock().expect("snapshot lock poisoned");
            let mut stats_cache = self.stats_cache.lock().expect("stats_cache lock poisoned");
            // Re-check under the same lock the merge happens under: a pinned
            // reader may have re-tagged the memo since `cached` was read.
            let ok = memo.version == base
                && plans.iter().all(|(scope, d, a)| {
                    match memo.map.get_mut(scope) {
                        Some(arc) => {
                            // Copy-on-write: a running query or the stats
                            // builder may still hold this snapshot. Cloning it
                            // (O(n)) beats the O(n log n) rebuild that
                            // `invalidate` would cost the next query.
                            let src = Arc::make_mut(arc);
                            // Stats first: `apply_delta` reads the *pre*-merge
                            // source to tell which rows the delta really
                            // changes. A summary that refuses the merge (drift
                            // spent) is dropped, and the next estimate
                            // rebuilds it.
                            let keep = match stats_cache.get_mut(scope) {
                                // A summary at another version (a pinned read
                                // view shares this cache) does not describe
                                // the rows this delta applies to.
                                Some((v, _)) if *v != base => false,
                                Some((v, StatsSlot::Ready(stats))) => {
                                    let ok = Arc::make_mut(stats).apply_delta(src, d, a);
                                    *v = post;
                                    ok
                                }
                                // A build in flight describes the pre-merge
                                // rows; it replays this delta when it lands.
                                Some((v, StatsSlot::Building { pending, .. })) => {
                                    if pending.len() >= STATS_PENDING_CAP {
                                        false
                                    } else {
                                        pending.push((d.clone(), a.clone()));
                                        *v = post;
                                        true
                                    }
                                }
                                None => true,
                            };
                            if !keep {
                                stats_cache.remove(scope);
                            }
                            src.apply_delta(d, a);
                            true
                        }
                        None => false,
                    }
                });
            if ok {
                memo.version = post;
            }
            ok
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
        let quad = self
            .store
            .dictionary()
            .intern_quad(g, s, p, o)
            .map_err(|e| SparqlError::Executor(format!("intern: {e}")))?;
        // SPARQL INSERT DATA is idempotent on an already-live triple. The
        // storage insert below reports that correctly on its own; this point
        // read is O(log rows) and keeps a repeated insert off the tier
        // entirely. Since HDB-102 the call it skips no longer rebuilds the
        // partition to reach the same answer — it probes the partition's runs
        // and appends nothing — so what this saves is smaller than it was,
        // but a read is still cheaper than a write.
        if self
            .store
            .snapshot()
            .contains_quad(g, quad.subject(), quad.predicate(), quad.object())
        {
            return Ok(false);
        }
        // Ids, not terms: the intern above is the only dictionary pass.
        let inserted = self
            .store
            .insert_quad_ids(&[quad])
            .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?;
        if inserted == 0 {
            return Ok(false);
        }
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
    /// * Phase 1 (read-only): intern every term into `entries`
    ///   ([`InternedQuad`]s), in input order, duplicates included. An intern
    ///   failure skips that triple.
    /// * Phase 2 (write): call `store.insert_quad_ids` once for `entries`.
    ///   Propagates storage errors.
    ///
    /// Neither already-live triples nor within-batch duplicates are filtered
    /// here: `Tier::apply_quad_batch` groups the add side per predicate and
    /// sorts + dedups it before deciding what is genuinely new, and is
    /// itself idempotent, returning the exact count of quads that became
    /// live — which is what this method returns. (`entries` used to be
    /// pre-filtered for in-batch duplicates too, via a `HashSet<QuadKey>`;
    /// HDB-104 removed that as redundant with storage's own dedup.)
    ///
    /// `entries` holds ids, not terms, so storage does not intern the same
    /// terms a second time.
    fn insert_oxrdf_batch_in_graph(
        &mut self,
        g: GraphId,
        triples: Vec<(oxrdf::Term, oxrdf::Term, oxrdf::Term)>,
    ) -> Result<u64> {
        if triples.is_empty() {
            return Ok(0);
        }

        // Phase 1 (read-only): intern every term. `entries` holds the
        // interned ids — it is the buffer phase 2 hands straight to storage,
        // duplicates and all; `apply_quad_batch` dedups the add side itself.
        let mut entries: Vec<InternedQuad> = Vec::with_capacity(triples.len());
        let n_in = triples.len() as u64;
        let t_dedupe = std::time::Instant::now();
        {
            let d = self.store.dictionary();
            for (s, p, o) in triples {
                let Ok(quad) = d.intern_quad(g, &s, &p, &o) else {
                    continue; // intern failure — skip this triple (lenient for bulk loads; the single-triple insert_oxrdf propagates instead)
                };
                entries.push(quad);
            }
        }

        record_load_phase(LoadPhase::Dedupe, t_dedupe.elapsed(), n_in);

        if entries.is_empty() {
            return Ok(0);
        }

        self.commit_quad_ids(&entries)
    }

    /// Phase 2 of every id-level bulk load: one storage call, then one
    /// snapshot invalidation. `entries` is already in the shape storage
    /// wants, so there is nothing to stage and nothing to re-intern. The
    /// returned count is the authoritative number of newly live quads —
    /// storage skips whatever was already visible.
    fn commit_quad_ids(&mut self, entries: &[InternedQuad]) -> Result<u64> {
        let inserted = self
            .store
            .insert_quad_ids(entries)
            .map_err(|e| SparqlError::Executor(format!("storage insert: {e}")))?
            as u64;

        if inserted == 0 {
            return Ok(0);
        }

        let t_inv = std::time::Instant::now();
        self.invalidate();
        record_load_phase(LoadPhase::Invalidate, t_inv.elapsed(), inserted);
        self.demote_all_if_cold_tier();

        Ok(inserted)
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

    /// Bulk-load a closure that is already in id form: `entries` gives every
    /// `(lexical key, id)` pair of the producer's dictionary (keys in the
    /// `Engine::materialized_triples()` convention), `triples` the closure as
    /// producer-side `(s, p, o)` ids.
    ///
    /// Interns once per *dictionary entry* rather than three times per triple
    /// — HDB-87's "intern once", applied to the reasoning path (HDB-117).
    /// A producer id with no entry, or whose key does not intern, drops the
    /// triples that use it (same skip-defensively stance as
    /// `Engine::materialized_triples`).
    pub fn load_id_closure<'a>(
        &mut self,
        entries: impl Iterator<Item = (&'a str, u64)>,
        triples: &[(u64, u64, u64)],
    ) -> Result<u64> {
        let t_dedupe = std::time::Instant::now();
        // Producer ids are dense and small, so a positional table beats a
        // hash map: one `Vec` index per closure term instead of a hash.
        let mut remap: Vec<Option<TermId>> = Vec::new();
        let quads: Vec<InternedQuad> = {
            let d = self.store.dictionary();
            for (key, id) in entries {
                let idx = id as usize;
                if idx >= remap.len() {
                    remap.resize(idx + 1, None);
                }
                remap[idx] = d.intern(&lexical_to_oxrdf(key)).ok();
            }
            triples
                .iter()
                .filter_map(|&(s, p, o)| {
                    let get = |id: u64| *remap.get(id as usize)?;
                    Some(d.quad_from_ids(DEFAULT_GRAPH, get(s)?, get(p)?, get(o)?))
                })
                .collect()
        };
        record_load_phase(LoadPhase::Dedupe, t_dedupe.elapsed(), triples.len() as u64);
        if quads.is_empty() {
            return Ok(0);
        }
        self.commit_quad_ids(&quads)
    }

    /// Interning `QuadKey` lookup: creates dictionary entries for any term
    /// `g`/`s`/`p`/`o` it has not seen. Used on the insert side of
    /// [`Self::apply_delta_to_snapshots`], where the terms were interned by
    /// the storage write just above, so this is a cheap hit in practice.
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
    /// side of [`Self::apply_delta_to_snapshots`], mirroring
    /// [`Self::intern_key`]'s interning lookup on the insert side.
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
            ResolvedScope::UnboundGraphVar(var) => {
                return Err(graph_var_needs_a_per_graph_node(var))
            }
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
            SnapshotScope::OneGraph(g) => self.snap().graph_len(*g) > 0,
            // An unknown or dataset-excluded graph name resolved to the empty
            // scope; the whole-store scopes are unreachable for a ground
            // `GRAPH <g>`.
            _ => false,
        }
    }

    /// The source one execution of `scope` reads from.
    ///
    /// Prefers [`StoreTripleSource`], which reads the columnar partitions in
    /// place, so nothing copies the store per query (HDB-120). Falls back to
    /// the memoised [`VecTripleSource`] for a scope the direct source cannot
    /// serve — see [`Self::direct_graph`] — and, by default, for every scope:
    /// the direct source is opt-in until a hornbench A/B says it should not be
    /// (see [`direct_source_enabled`] and the `serving footprint` row in
    /// `docs/benchmarks.md`).
    fn query_source(&self, scope: &SnapshotScope) -> QuerySource {
        if self.direct_source {
            if let Some(g) = self.direct_graph(scope) {
                return QuerySource::Direct(self.direct_source_for(g));
            }
        }
        QuerySource::Copy(self.wcoj_snapshot(scope))
    }

    /// A [`StoreTripleSource`] over `graph` at the version this backend reads
    /// at, reusing the cached one when both still match — see `direct_cache`.
    ///
    /// Goes through [`Self::snap`], so a pinned read view (HDB-119) opens the
    /// source over *its* tier state, not the store's latest: the returned
    /// `Arc<TierSnapshot>` is the pinned one, and its `version()` is the
    /// pinned commit version, which is what keys the cache.
    fn direct_source_for(&self, graph: GraphId) -> Arc<StoreTripleSource> {
        let tier = self.snap().tier_arc();
        let version = tier.version();
        let mut guard = self.direct_cache.lock().expect("direct cache poisoned");
        if let Some((v, g, src)) = guard.as_ref() {
            if *v == version && *g == graph {
                return Arc::clone(src);
            }
        }
        let src = Arc::new(StoreTripleSource::new(tier, graph));
        *guard = Some((version, graph, Arc::clone(&src)));
        src
    }

    /// The single graph `scope` reads, if it has one.
    ///
    /// [`StoreTripleSource`] merges one leaf per predicate and needs those
    /// leaf keys distinct, which holds within one graph but not across a
    /// union of several — see `store_source`'s module docs. `DefaultUnion`
    /// counts as single-graph exactly when at most one graph visible to it
    /// holds data (see [`hidden_reserved`]): the single-tenant shape, and
    /// every trainmarks/SPB run.
    fn direct_graph(&self, scope: &SnapshotScope) -> Option<GraphId> {
        match scope {
            SnapshotScope::DefaultStrict => Some(DEFAULT_GRAPH),
            SnapshotScope::OneGraph(g) => Some(*g),
            SnapshotScope::DefaultUnion => {
                let snap = self.snap();
                let mut live = snap
                    .graphs()
                    .into_iter()
                    .filter(|g| !hidden_reserved(&snap, *g, &self.visible_inferred));
                match (live.next(), live.next()) {
                    // No graph holds data: any graph id reads empty.
                    (None, _) => Some(DEFAULT_GRAPH),
                    (Some(g), None) => Some(g),
                    _ => None,
                }
            }
            // A one-graph `FROM` already resolved to `OneGraph`; what is left
            // is the empty set (cheap either way) or a real multi-graph union.
            SnapshotScope::FromUnion(_) => None,
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
        // Hit, or a twin worth cloning from, decided under one lock
        // acquisition so it is atomic with whatever is cached right now.
        // `DefaultStrict` and `DefaultUnion` read the same triples whenever
        // no named graph besides the default-graph sentinel holds data
        // (HDB-97): a single-tenant store, and trainmarks-shaped workloads
        // generally. SPARQL Update's WHERE clause resolves `DefaultStrict`
        // while plain `SELECT`/`CONSTRUCT` resolve `DefaultUnion` (see
        // `apply_delete_insert` / `ScanScope::DEFAULT`), so a fresh store's
        // first update and first query used to each pay a full six-sort-pass
        // build for what is, in that common shape, the identical source.
        //
        // The clone only happens once *this* scope is actually asked for and
        // a same-content twin is already cached — never eagerly when the
        // twin itself is built. Deferring it this way means a workload that
        // only ever touches one of the two scopes (e.g. `q6`'s three warm
        // re-runs, which precede any `SELECT` in the trainmarks driver) never
        // pays to keep an unused twin's delta merged; it starts paying only
        // once a second scope is actually read.
        let version = self.read_version();
        let twin_src: Option<Arc<VecTripleSource>> = {
            let guard = self.snapshots.lock().expect("snapshot lock poisoned");
            // Entries built at another commit version answer another store
            // state — never reusable here (HDB-119).
            if guard.version != version {
                None
            } else {
                if let Some(s) = guard.map.get(scope) {
                    return Arc::clone(s);
                }
                scope
                    .default_twin()
                    .and_then(|twin| guard.map.get(&twin).cloned())
            }
        };
        // Build (or clone) with the lock RELEASED: neither a six-sort-pass
        // rebuild nor an O(n) clone of the twin's already-sorted data must
        // stall a concurrent reader whose own scope is already cached
        // (readers do run concurrently — `server/query.rs`). A race
        // duplicates the work and `or_insert` keeps the first result; the
        // two are interchangeable, since a write needs `&mut self` and so
        // cannot interleave with any read. The equivalence is checked here,
        // not assumed from when the twin was built — needed to still hold at
        // this instant, not merely at some earlier one (though in practice
        // any write that would break it invalidates the whole memo, so there
        // is never a stale twin to clone from in the first place).
        let built = match twin_src {
            Some(twin) if self.default_scopes_coincide() => (*twin).clone(),
            _ => VecTripleSource::from_triples(self.scope_triples(scope)),
        };
        let mut guard = self.snapshots.lock().expect("snapshot lock poisoned");
        if guard.version > version {
            // Someone reading a newer store owns the memo now; this build is
            // still correct for *this* reader, it just does not go in.
            return Arc::new(built);
        }
        if guard.version < version {
            guard.map.clear();
            guard.version = version;
        }
        Arc::clone(guard.map.entry(scope.clone()).or_insert(Arc::new(built)))
    }

    /// True when [`SnapshotScope::DefaultStrict`] and [`SnapshotScope::DefaultUnion`]
    /// cover the exact same triples for the store's current graph shape: no
    /// non-reserved named graph holds data besides the default-graph sentinel
    /// itself. See [`Self::wcoj_snapshot`]'s twin pre-warm.
    fn default_scopes_coincide(&self) -> bool {
        let snap = self.snap();
        snap.graphs()
            .into_iter()
            .filter(|g| !hidden_reserved(&snap, *g, &self.visible_inferred))
            .all(|g| g == DEFAULT_GRAPH)
    }

    /// Every `(s, p, o)` id-triple visible in `scope`, from one pinned store
    /// snapshot. Multi-graph scopes are a **set** union — the same triple in
    /// two graphs is one row of the union graph (SPEC-28 S3) — enforced by
    /// the snapshot builder's dedup; see [`union_triples`].
    fn scope_triples(&self, scope: &SnapshotScope) -> Vec<WTriple> {
        let snap = self.snap();
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
                    .filter(|g| !hidden_reserved(&snap, *g, &self.visible_inferred))
                    .collect();
                union_triples(&snap, &graphs)
            }
        }
    }

    /// Number of memoised snapshots. Test-only window on the cache that
    /// `graph_scoped_snapshots_are_not_memoised` bounds.
    #[cfg(test)]
    fn memo_len(&self) -> usize {
        self.snapshots
            .lock()
            .expect("snapshot lock poisoned")
            .map
            .len()
    }

    /// Statistics the join planner reads for `source`. A direct store source
    /// has no per-predicate counts, so it gets [`ZeroStats`] and the planner
    /// routes structurally (one WCOJ node in degree order). A copied snapshot
    /// gets its cached [`SnapshotStats`] when one exists at this version;
    /// otherwise a build is started on a background thread (marked by a
    /// [`StatsSlot::Building`] entry) and the query plans with [`ZeroStats`]
    /// meanwhile — the query path never pays the snapshot scan.
    ///
    /// Cold-start residual: queries issued while the build runs (tens of
    /// milliseconds per million triples) get the structural plan. A write
    /// landing in that window copies the snapshot (the builder keeps the
    /// pre-merge one) and queues its delta on the slot for the builder to
    /// replay (at most [`STATS_PENDING_CAP`] of them), so the memo and the
    /// summary both survive. A build starts only when no `Building` entry is
    /// present, which keeps concurrent builds rare, not impossible; a full
    /// cache is cleared like `snapshot_stats` does.
    fn planning_stats(&self, scope: &SnapshotScope, source: &QuerySource) -> Arc<dyn Stats> {
        let snapshot = match source {
            QuerySource::Copy(vec) => vec,
            QuerySource::Direct(direct) => {
                return Arc::new(ZeroStats::new(direct.total_triples() as u64))
            }
        };
        let version = self.read_version();
        let mut guard = self.stats_cache.lock().expect("stats cache lock poisoned");
        match guard.get(scope) {
            Some((v, StatsSlot::Ready(stats))) if *v == version => {
                let stats: Arc<dyn Stats> = Arc::<SnapshotStats>::clone(stats);
                return stats;
            }
            Some((v, StatsSlot::Building { .. })) if *v == version => {} // in flight
            _ => {
                guard.retain(|_, (v, _)| *v == version);
                // One build at a time bounds the thread count; the next query
                // on another scope starts its own once this one lands.
                // ponytail: a slot dropped mid-build (invalidate, clear) lets a
                // second thread start before the first exits; both are
                // O(store) and the stale one discards itself in `land_stats`.
                let busy = guard
                    .values()
                    .any(|(_, s)| matches!(s, StatsSlot::Building { .. }));
                if !busy {
                    if guard.len() >= STATS_CACHE_MAX_SCOPES {
                        guard.clear();
                    }
                    static BUILD_ID: AtomicU64 = AtomicU64::new(0);
                    let id = BUILD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    guard.insert(
                        scope.clone(),
                        (
                            version,
                            StatsSlot::Building {
                                id,
                                pending: Vec::new(),
                            },
                        ),
                    );
                    let cache = Arc::clone(&self.stats_cache);
                    let snapshot = Arc::clone(snapshot);
                    let key = scope.clone();
                    let spawned = std::thread::Builder::new()
                        .name("horndb-stats".into())
                        .spawn(move || {
                            let stats = build_stats(&snapshot);
                            let mut guard = cache.lock().expect("stats cache lock poisoned");
                            land_stats(&mut guard, &key, id, snapshot, stats);
                        });
                    if spawned.is_err() {
                        guard.remove(scope);
                    }
                }
            }
        }
        Arc::new(ZeroStats::new(snapshot.total_triples() as u64))
    }

    /// Get-or-build the [`SnapshotStats`] summary for `scope` synchronously
    /// (EXPLAIN needs the real estimates now), caching it against the commit
    /// version this backend reads at (see the `stats_cache` field) — the
    /// pinned version on a read view, the latest committed state on the
    /// writable backend. A hit costs a hash lookup; a miss is a full snapshot
    /// scan, counted and timed as `horndb_sparql_stats_rebuild`.
    fn snapshot_stats(
        &self,
        scope: &SnapshotScope,
        snapshot: &Arc<VecTripleSource>,
    ) -> Arc<SnapshotStats> {
        let version = self.read_version();
        let mut guard = self.stats_cache.lock().expect("stats cache lock poisoned");
        if let Some((v, StatsSlot::Ready(stats))) = guard.get(scope) {
            if *v == version {
                return Arc::clone(stats);
            }
        }
        let stats = build_stats(snapshot);

        // Bound the cache: an entry at another version is already dead, and a
        // client naming many `GRAPH <g>` scopes in one version must not grow
        // it without limit (the sink `graph_scoped_snapshots_are_not_memoised`
        // bounds for snapshots).
        // ponytail: clear-when-full, not LRU. Ceiling is a cold rebuild for the
        // hot scopes right after a wide `GRAPH` sweep; make it an LRU if that
        // ever shows up in a profile.
        guard.retain(|_, (v, _)| *v == version);
        if guard.len() >= STATS_CACHE_MAX_SCOPES {
            guard.clear();
        }
        guard.insert(
            scope.clone(),
            (version, StatsSlot::Ready(Arc::clone(&stats))),
        );
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

/// O(1): shares the storage handle and the snapshot memo, pins one commit
/// version. See [`HornBackend::pin_read`].
impl Pinnable for HornBackend {
    type View = HornBackend;

    fn pin_read(&self) -> HornBackend {
        HornBackend::pin_read(self)
    }
}

impl Store for HornBackend {
    /// Resolves `dels` and `adds` against this store's dictionary (dels
    /// non-interning — an unseen graph/term retracts nothing; adds
    /// interning, so a never-seen named graph or term is created), then
    /// applies both in one call to `horndb_storage::Store::apply_quads`
    /// (Task 1, SPEC-28 S6) — the atomic dels-before-adds, idempotent,
    /// counted store boundary. The returned `retracted`/`inserted` are
    /// storage's own counts, so `DELETE DATA` no-op detection and `INSERT
    /// DATA` idempotency are decided by the store and by nothing above it.
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

        debug_assert!(self.pin.is_none(), "write through a pinned read view");
        #[cfg(feature = "incremental")]
        let flips = match self.circuit {
            Some(_) => Some(self.default_graph_flips(&del_rows, &add_rows)?),
            None => None,
        };
        let base = self.store.snapshot().version();
        let report = self
            .store
            .apply_quads(&del_rows, &add_rows)
            .map_err(|e| SparqlError::Executor(format!("storage apply_quads: {e}")))?;

        // SPEC-24 S4: one circuit tick per Update operation, right after the
        // operation's batch committed. The derived mirror is a second commit
        // in another graph, which the memo delta below cannot express, so a
        // tick that changed anything drops the memo instead.
        #[cfg(feature = "incremental")]
        let derived_changed = match (&mut self.circuit, flips) {
            (Some(wiring), Some((asserts, retracts))) => {
                let derived = wiring.apply(&self.store, &asserts, &retracts);
                derived.inserted > 0 || derived.retracted > 0
            }
            _ => false,
        };
        #[cfg(not(feature = "incremental"))]
        let derived_changed = false;

        let changed = report.retracted > 0 || report.inserted > 0;
        if derived_changed {
            self.invalidate();
        } else if changed {
            self.apply_delta_to_snapshots(base, &del_rows, &add_rows);
        }
        if changed {
            // SPEC-29 D7: record what changed for the view router. Gated on
            // the batch having changed something, so replaying an identical
            // change-feed batch marks nothing dirty and derives nothing.
            self.touched_graphs
                .extend(del_rows.iter().chain(add_rows.iter()).map(|(g, ..)| *g));
            self.demote_all_if_cold_tier();
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
        debug_assert!(self.pin.is_none(), "write through a pinned read view");
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
        let dict = self.store.dictionary();
        let dels: Vec<InternedQuad> = graphs_to_sweep
            .iter()
            .flat_map(|&g| {
                snap.iter_graph_term_ids(g)
                    .map(move |(s, p, o)| dict.quad_from_ids(g, s, p, o))
            })
            .collect();
        if dels.is_empty() {
            return Ok(0);
        }
        // SPEC-24 S4: the default-graph rows this sweep removes are the
        // circuit's retracts (every one is present, so no before/after
        // check is needed).
        #[cfg(feature = "incremental")]
        let retracts: Vec<horndb_incremental::TripleId> =
            if self.circuit.is_some() && graphs_to_sweep.contains(&DEFAULT_GRAPH) {
                snap.iter_graph_term_ids(DEFAULT_GRAPH)
                    .map(|(s, p, o)| (s.0, p.0, o.0))
                    .collect()
            } else {
                Vec::new()
            };
        // Through the store, not the tier: the write-ahead log must see the
        // sweep, or the next logged batch cannot replay (SPEC-25 S3).
        let report = self
            .store
            .apply_quad_ids(&dels, &[])
            .map_err(|e| SparqlError::Executor(format!("clear_graph: {e}")))?;
        if report.retracted > 0 {
            #[cfg(feature = "incremental")]
            if let Some(wiring) = &mut self.circuit {
                if !retracts.is_empty() {
                    wiring.apply(&self.store, &[], &retracts);
                }
            }
            self.invalidate();
            // SPEC-29 D7: `CLEAR`/`DROP` bypasses `apply_quads` (it sweeps
            // one tier level down), so it reports its own touched graphs.
            self.touched_graphs.extend(graphs_to_sweep.iter().copied());
            self.demote_all_if_cold_tier();
        }
        Ok(report.retracted)
    }

    /// Point read against the pinned snapshot
    /// (`horndb_storage::StoreSnapshot::contains_quad`). Resolution is
    /// non-interning on every side — an unseen graph or term means the quad
    /// simply is not there, so nothing is created just to answer a read.
    fn quad_exists(&self, (g, s, p, o): &AlgebraQuad) -> bool {
        let Some(gid) = self.resolve_graph_for_delete(g.as_deref()) else {
            return false;
        };
        let (Ok(so), Ok(po), Ok(oo)) = (
            algebra_to_oxrdf(s),
            algebra_to_oxrdf(p),
            algebra_to_oxrdf(o),
        ) else {
            return false; // a variable / RDF 1.2 triple term is never stored
        };
        let dict = self.store.dictionary();
        let (Some(si), Some(pi), Some(oi)) = (dict.get(&so), dict.get(&po), dict.get(&oo)) else {
            return false;
        };
        self.store.snapshot().contains_quad(gid, si, pi, oi)
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

    /// Overrides the trait default (a process-wide counter) to scope the tag
    /// to this backend's own store, matching the bulk loaders (HDB-113).
    fn next_bnode_doc_tag(&self) -> u64 {
        HornBackend::next_bnode_doc_tag(self)
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

        let snapshot = self.query_source(&resolved);
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
            &snapshot,
            &bgp,
            &Planner::default(),
            self.planning_stats(&resolved, &snapshot).as_ref(),
            crate::exec::cancel::current(),
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
    /// scan the `PerGraph` operator then runs pins another
    /// (`scope_triples`), so one
    /// `GRAPH ?g` reads N+1 pinned views rather than the single one SPEC-28
    /// S2 describes. They cannot disagree today — every write takes
    /// `&mut self` and no read holds it, so no write can interleave with a
    /// query. Threading one snapshot through would have to widen the whole
    /// `Executor` read seam (`scan_bgp_ids` and friends take no snapshot),
    /// which is out of proportion to a difference that is currently
    /// unobservable; revisit when writes become concurrent with reads.
    fn named_graphs(&self, named: Option<&[String]>) -> Result<Vec<NamedGraph>> {
        let snap = self.snap();
        let mut out: Vec<NamedGraph> = Vec::new();
        for g in snap.graphs() {
            // `graph_uri` errors on DEFAULT_GRAPH (a sentinel with no IRI),
            // which is also exactly the graph `GRAPH ?g` must never bind.
            let Ok(OxTerm::NamedNode(n)) = snap.graph_uri(g) else {
                continue;
            };
            let iri = n.into_string();
            let admitted = match named {
                // No `FROM NAMED`: every non-reserved graph, plus any
                // reserved graph SPEC-29 D6's flag opted back in.
                None => !is_reserved_graph(&iri) || self.visible_inferred.contains(&iri),
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

    /// HDB-100: reads the dictionary's stored `oxrdf::Literal` value in
    /// place under one lock — no `Term` clone, no `to_string`, no unescape,
    /// no re-parse. See `Dictionary::numeric_value`.
    fn decode_numeric(&self, id: TermId) -> Result<Option<f64>> {
        Ok(self.store.dictionary().numeric_value(id))
    }

    /// HDB-100: batches through `Dictionary::lookup_batch`, which already
    /// takes the reverse map's read lock once for the whole slice.
    fn decode_terms(&self, ids: &[TermId]) -> Result<Vec<Term>> {
        let dict = self.store.dictionary();
        dict.lookup_batch(ids)
            .into_iter()
            .zip(ids)
            .map(|(t, id)| {
                t.map(|t| oxrdf_to_algebra(&t))
                    .ok_or_else(|| SparqlError::Executor(format!("dangling TermId {id:?}")))
            })
            .collect()
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

        let snapshot = self.query_source(&resolved);
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
        // HDB-99: timed by hand rather than via `crate::exec::phases::timed`
        // because the phase's `rows` (the arrow batch's row count) is only
        // known from the value the iterator yields — `enabled()` gates the
        // `Instant::now()` pair so the check costs one branch per arrow
        // batch when the flag is off, never a clock read.
        let mut wcoj_iter = WcojExecutor::for_bgp(
            &snapshot,
            &bgp,
            &Planner::default(),
            self.planning_stats(&resolved, &snapshot).as_ref(),
            crate::exec::cancel::current(),
        );
        loop {
            let scan_t0 = crate::exec::phases::enabled().then(std::time::Instant::now);
            let next = wcoj_iter.next();
            let Some(batch) = next else { break };
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let batch_rows = batch.num_rows() as u64;
            if let Some(t0) = scan_t0 {
                crate::exec::phases::add(
                    ExecPhase::ScanWcoj,
                    t0.elapsed().as_nanos() as u64,
                    batch_rows,
                );
            }
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
            // One pair per arrow batch, not per row (SPEC-17 §5.3/§5.4).
            crate::exec::phases::timed(ExecPhase::ScanRowBuild, batch_rows, || {
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
            });
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
        let resolved = self.resolve_scope(scope).ok()?;
        let snapshot = self.wcoj_snapshot(&resolved);
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
        // so cache it per scope, tagged with the store's commit version: an
        // `EXPLAIN` with many BgpScan/GroupCountScan nodes calls this once per
        // node at one version — see the `stats_cache` field.
        let stats = self.snapshot_stats(&resolved, &snapshot);
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
        // The empty BGP is the join identity: one solution — zero when the
        // scope is a ground `GRAPH <g>` the dataset does not have.
        if patterns.is_empty() {
            return Ok(Some(usize::from(
                self.empty_group_matches(scope, &resolved),
            )));
        }

        let snapshot = self.query_source(&resolved);
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
            &snapshot,
            &bgp,
            &Planner::default(),
            self.planning_stats(&resolved, &snapshot).as_ref(),
            crate::exec::cancel::current(),
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

        let snapshot = self.query_source(&resolved);
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
            &snapshot,
            &bgp,
            &Planner::default(),
            self.planning_stats(&resolved, &snapshot).as_ref(),
            crate::exec::cancel::current(),
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

        // Same bound for `GRAPH ?g`: the `PerGraph` operator reads one
        // graph at a time through this same ground path, so the memo stays
        // at the one whole-store entry however many graphs it walks.
        let mut rows = 0;
        for g in b.named_graphs(None).unwrap() {
            let scope = GraphScope::Named(GraphSpec::Iri(g.iri));
            let scope = ScanScope::new(&scope, &dataset, crate::DefaultGraphMode::Union);
            rows += b.scan_bgp_ids(&patterns, &scope).unwrap().rows.len();
        }
        assert_eq!(rows, 20, "one row per graph");
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

    /// HDB-104: `insert_oxrdf_batch_in_graph` no longer dedups within the
    /// batch itself (`intra_batch` is gone) — a duplicated triple now reaches
    /// `Tier::apply_quad_batch`, which groups the add side per predicate and
    /// sorts + dedups it (HDB-88) before deciding what is genuinely new. This
    /// must still report exactly one newly-live quad, and the store must
    /// still hold exactly one copy.
    #[test]
    fn a_batch_carrying_the_same_quad_twice_inserts_it_once() {
        let mut b = HornBackend::new();
        let s = OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/s"));
        let p = OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/p"));
        let o = OxTerm::NamedNode(NamedNode::new_unchecked("http://ex/o"));
        let n = b
            .insert_oxrdf_batch(vec![(s.clone(), p.clone(), o.clone()), (s, p, o)])
            .unwrap();
        assert_eq!(n, 1, "a duplicated triple counts as one newly-live quad");
        assert_eq!(b.len(), 1, "the store holds exactly one copy");
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

    /// A small update must keep the memoised snapshot ALIVE (merged in place)
    /// rather than silently rotting into "always fall back to a full
    /// rebuild" -- which would still pass every correctness test above while
    /// throwing away the whole point of PLAN-03-03. Same test-only
    /// `memo_len` window `graph_scoped_snapshots_are_not_memoised` uses,
    /// plus `Arc::ptr_eq` to prove the *same* snapshot object was mutated in
    /// place, not dropped and rebuilt as a fresh `Arc`.
    #[test]
    fn small_update_retains_and_merges_the_snapshot_cache() {
        let mut b = HornBackend::new();
        for i in 0..10 {
            b.insert_triple(
                Term::Iri(format!("http://ex/s{i}")),
                Term::Iri("http://ex/p".into()),
                Term::Iri(format!("http://ex/o{i}")),
            );
        }

        // Warm the one memoisable entry (`SnapshotScope::DefaultUnion`, what
        // a bare BGP resolves to — see `resolve_scope`). Asked for directly:
        // since HDB-120 a plain read takes the direct partition source, so
        // only `EXPLAIN` and multi-graph scopes still populate this memo.
        // Nothing has asked for the `DefaultStrict` twin yet, so it is not
        // cloned in eagerly (HDB-97's twin-clone-on-miss is lazy — see
        // `wcoj_snapshot`).
        let _ = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
        assert_eq!(b.memo_len(), 1, "warm-up: one memoised scope");
        // Capture the snapshot's identity as a raw pointer, then drop the
        // `Arc` clone: `apply_delta_to_snapshots` merges in place only when
        // the cache holds the sole strong reference -- a clone kept alive here
        // would make it copy on write, and the pointer check below would say
        // nothing about the in-place path.
        let before_ptr = {
            let snap = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
            assert_eq!(snap.total_triples(), 10);
            Arc::as_ptr(&snap)
        };

        // One more matching triple: well under the `SNAPSHOT_DELTA_REBUILD_
        // DIVISOR` threshold (1 <= 10 / 2), so the fast path applies.
        b.insert_triple(
            Term::Iri("http://ex/s10".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/o10".into()),
        );

        assert_eq!(
            b.memo_len(),
            1,
            "a small delta must merge in place, not drop the memo"
        );
        let after = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
        assert_eq!(
            Arc::as_ptr(&after),
            before_ptr,
            "the fast path mutates the existing Arc rather than rebuilding a fresh one"
        );
        assert_eq!(
            after.total_triples(),
            11,
            "the merged snapshot reflects the new row"
        );
    }

    /// HDB-97: a store with only the default graph makes `DefaultStrict` and
    /// `DefaultUnion` read the same triples (SPARQL Update's WHERE clause
    /// resolves `DefaultStrict`, plain `SELECT`/`CONSTRUCT` resolve
    /// `DefaultUnion` -- see `apply_delete_insert` / `ScanScope::DEFAULT`).
    /// Once one is built, asking for the other must clone it (cheap) rather
    /// than pay its own six-sort-pass rebuild -- but only once it is
    /// actually asked for; building the first one alone must not eagerly
    /// warm the second (see `wcoj_snapshot`'s lazy clone-on-miss).
    #[test]
    fn default_scope_twin_clones_instead_of_rebuilding_once_asked_for() {
        let mut b = HornBackend::new();
        for i in 0..10 {
            b.insert_triple(
                Term::Iri(format!("http://ex/s{i}")),
                Term::Iri("http://ex/p".into()),
                Term::Iri(format!("http://ex/o{i}")),
            );
        }

        let strict = b.wcoj_snapshot(&SnapshotScope::DefaultStrict);
        assert_eq!(
            b.memo_len(),
            1,
            "building DefaultStrict alone must not eagerly warm its twin"
        );

        let union = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
        assert_eq!(
            b.memo_len(),
            2,
            "asking for the twin now caches it too (via clone, not rebuild)"
        );
        assert_eq!(union.total_triples(), 10, "twin has the same rows");
        assert!(
            !Arc::ptr_eq(&strict, &union),
            "each scope must own an independent Arc, so a later delta merge \
             into one is in place, not a copy forced by the other's strong ref"
        );
    }

    /// The twin clone must not fire when a named graph holds data besides
    /// the default graph: there `DefaultStrict` and `DefaultUnion` genuinely
    /// read different triples, so cloning one into the other would cache a
    /// wrong answer.
    #[test]
    fn default_scope_twin_clone_skipped_when_graphs_diverge() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/s".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/default".into()),
        );
        let iri = |v: &str| OxTerm::NamedNode(NamedNode::new_unchecked(v));
        b.insert_oxrdf_in_named_graph(
            &iri("http://ex/g0"),
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/named"),
        )
        .unwrap();

        let strict = b.wcoj_snapshot(&SnapshotScope::DefaultStrict);
        assert_eq!(b.memo_len(), 1, "only DefaultStrict is cached so far");
        assert_eq!(
            strict.total_triples(),
            1,
            "DefaultStrict sees only the default graph"
        );

        let union = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
        assert_eq!(
            union.total_triples(),
            2,
            "DefaultUnion sees both graphs -- genuinely not DefaultStrict's twin here"
        );
    }

    /// Every `(?s, ?p, ?o)` row over the default-union scope, independent of
    /// row order. Mirrors `tests/incremental_snapshot_delta.rs`'s `all_spo`.
    fn all_spo(b: &HornBackend) -> HashSet<(Term, Term, Term)> {
        let crate::api::QueryAnswer::Solutions { rows, .. } =
            crate::api::execute_query("SELECT ?s ?p ?o WHERE { ?s ?p ?o }", b).unwrap()
        else {
            panic!("expected solutions");
        };
        rows.iter()
            .map(|r| {
                (
                    r.get("s").unwrap().clone(),
                    r.get("p").unwrap().clone(),
                    r.get("o").unwrap().clone(),
                )
            })
            .collect()
    }

    /// Warm the memoised `DefaultUnion` snapshot, apply `update`, then assert
    /// the cache took the merge path -- same `memo_len` + `Arc::as_ptr`
    /// identity check as `small_update_retains_and_merges_the_snapshot_cache`.
    fn assert_update_merges_in_place(b: &mut HornBackend, update: &str) {
        let before_ptr = Arc::as_ptr(&b.wcoj_snapshot(&SnapshotScope::DefaultUnion));
        crate::update::apply_update(&crate::parser::parse_update(update).unwrap(), b).unwrap();
        assert_eq!(
            b.memo_len(),
            1,
            "a warmed cache must survive the update via the merge path, not be dropped"
        );
        let after = b.wcoj_snapshot(&SnapshotScope::DefaultUnion);
        assert_eq!(
            Arc::as_ptr(&after),
            before_ptr,
            "the update must merge into the existing Arc, not fall back to invalidate+rebuild"
        );
    }

    /// A small write must not cost the next query a full `SnapshotStats`
    /// rebuild (HDB-123): `apply_delta_to_snapshots` merges the same quad
    /// delta into the cached summary and re-tags it with the new commit
    /// version. Asserted on the rebuild counter rather than on wall-clock,
    /// which would be flaky. No other unit test in this crate builds planner
    /// statistics, so the process-global counter is this test's alone.
    #[test]
    fn small_write_merges_planner_stats_instead_of_rebuilding() {
        let rebuilds = || horndb_metrics::metrics().sparql.stats_rebuild.get();
        // 2000 rows so that one added row stays far under the drift bound
        // (`STATS_DRIFT_DIVISOR`), which is what keeps the merge path live.
        let mut b = HornBackend::new();
        for i in 0..2000 {
            b.insert_triple(
                Term::Iri(format!("http://ex/s{i}")),
                Term::Iri("http://ex/p".into()),
                Term::Iri(format!("http://ex/o{}", i % 7)),
            );
        }
        let patterns = vec![TriplePattern {
            subject: Term::Var(Var::new("s")),
            predicate: Term::Iri("http://ex/p".into()),
            object: Term::Var(Var::new("o")),
        }];

        let base = rebuilds();
        let e0 = b
            .cardinality_estimate(&patterns, &ScanScope::DEFAULT)
            .unwrap();
        assert_eq!(
            rebuilds(),
            base + 1,
            "the first estimate builds the summary"
        );
        assert_eq!(
            b.cardinality_estimate(&patterns, &ScanScope::DEFAULT),
            Some(e0),
            "a second estimate at the same version is a cache hit"
        );
        assert_eq!(rebuilds(), base + 1, "...and rebuilds nothing");

        assert_update_merges_in_place(
            &mut b,
            "INSERT DATA { <http://ex/new> <http://ex/p> <http://ex/o0> }",
        );
        let e1 = b
            .cardinality_estimate(&patterns, &ScanScope::DEFAULT)
            .unwrap();
        assert_eq!(
            rebuilds(),
            base + 1,
            "the query path after a small write must not rebuild the summary"
        );
        assert_eq!(e1, e0 + 1, "the merged summary must see the new row");
    }

    fn two_thousand_rows() -> HornBackend {
        let mut b = HornBackend::new();
        for i in 0..2000 {
            b.insert_triple(
                Term::Iri(format!("http://ex/s{i}")),
                Term::Iri("http://ex/p".into()),
                Term::Iri(format!("http://ex/o{}", i % 7)),
            );
        }
        b
    }

    /// A write that lands while the `horndb-stats` builder (or any reader)
    /// still holds the memoised snapshot must not drop the memo: the merge
    /// path copies on write, queues the delta on the in-flight slot, and the
    /// builder replays it when it lands -- one build, no rebuild.
    #[test]
    fn write_during_stats_build_keeps_memo_and_lands_merged_stats() {
        let rebuilds = || horndb_metrics::metrics().sparql.stats_rebuild.get();
        let mut b = two_thousand_rows();
        let scope = SnapshotScope::DefaultUnion;
        // The builder's view: the pre-merge snapshot, pinned outside the memo.
        let pinned = b.wcoj_snapshot(&scope);
        let version = b.read_version();
        b.stats_cache.lock().unwrap().insert(
            scope.clone(),
            (
                version,
                StatsSlot::Building {
                    id: 7,
                    pending: Vec::new(),
                },
            ),
        );
        let base = rebuilds();
        crate::update::apply_update(
            &crate::parser::parse_update(
                "INSERT DATA { <http://ex/new> <http://ex/p> <http://ex/o0> }",
            )
            .unwrap(),
            &mut b,
        )
        .unwrap();
        assert_eq!(b.memo_len(), 1, "the memo survives a write during a build");
        assert_eq!(rebuilds(), base, "the write itself builds nothing");
        let now = b.wcoj_snapshot(&scope);
        assert_ne!(
            Arc::as_ptr(&now),
            Arc::as_ptr(&pinned),
            "copy-on-write: the pinned snapshot is left alone"
        );
        assert_eq!(pinned.total_triples(), 2000);
        assert_eq!(now.total_triples(), 2001);
        {
            let guard = b.stats_cache.lock().unwrap();
            match guard.get(&scope) {
                Some((v, StatsSlot::Building { pending, .. })) => {
                    assert_eq!(*v, b.read_version(), "slot re-tagged to the new version");
                    assert_eq!(pending.len(), 1, "the delta is queued for the builder");
                }
                _ => panic!("the in-flight slot must survive the write"),
            }
        }
        // The builder finishes on its pre-merge snapshot and lands.
        let stats = build_stats(&pinned);
        land_stats(&mut b.stats_cache.lock().unwrap(), &scope, 7, pinned, stats);
        let patterns = vec![TriplePattern {
            subject: Term::Var(Var::new("s")),
            predicate: Term::Iri("http://ex/p".into()),
            object: Term::Var(Var::new("o")),
        }];
        let e = b
            .cardinality_estimate(&patterns, &ScanScope::DEFAULT)
            .unwrap();
        assert_eq!(e, 2001, "the landed summary carries the replayed delta");
        assert_eq!(rebuilds(), base + 1, "one build in total");
    }

    fn building_slot(id: u64) -> StatsSlot {
        StatsSlot::Building {
            id,
            pending: Vec::new(),
        }
    }

    fn insert_one(b: &mut HornBackend, i: usize) {
        crate::update::apply_update(
            &crate::parser::parse_update(&format!(
                "INSERT DATA {{ <http://ex/new{i}> <http://ex/p> <http://ex/o0> }}"
            ))
            .unwrap(),
            b,
        )
        .unwrap();
    }

    /// A write stream during a build queues at most `STATS_PENDING_CAP`
    /// deltas; past that the slot is dropped (one ordinary rebuild later)
    /// instead of an unbounded replay under the cache lock. The memo itself
    /// keeps merging either way.
    #[test]
    fn write_stream_during_stats_build_drops_the_slot_past_the_cap() {
        let rebuilds = || horndb_metrics::metrics().sparql.stats_rebuild.get();
        let mut b = two_thousand_rows();
        let scope = SnapshotScope::DefaultUnion;
        let _pinned = b.wcoj_snapshot(&scope);
        let version = b.read_version();
        b.stats_cache
            .lock()
            .unwrap()
            .insert(scope.clone(), (version, building_slot(1)));
        let base = rebuilds();
        for i in 0..STATS_PENDING_CAP {
            insert_one(&mut b, i);
            let guard = b.stats_cache.lock().unwrap();
            match guard.get(&scope) {
                Some((_, StatsSlot::Building { pending, .. })) => assert_eq!(pending.len(), i + 1),
                _ => panic!("slot must survive write {i}"),
            }
        }
        insert_one(&mut b, STATS_PENDING_CAP);
        assert!(
            b.stats_cache.lock().unwrap().get(&scope).is_none(),
            "past the cap the in-flight slot is dropped"
        );
        assert_eq!(b.memo_len(), 1, "the snapshot memo keeps merging");
        assert_eq!(
            b.wcoj_snapshot(&scope).total_triples() as usize,
            2000 + STATS_PENDING_CAP + 1
        );
        assert_eq!(
            rebuilds(),
            base,
            "dropping the slot builds nothing by itself"
        );
    }

    /// A stats entry tagged at a version other than the write's base (a
    /// pinned read view shares the cache and can install one) must be
    /// dropped, never merged into and re-stamped as current.
    #[test]
    fn stats_at_another_version_are_dropped_not_merged() {
        let mut b = two_thousand_rows();
        let scope = SnapshotScope::DefaultUnion;
        let pinned = b.wcoj_snapshot(&scope);
        let foreign = b.read_version() + 1000;
        let ready = StatsSlot::Ready(Arc::new(SnapshotStats::from_source(&pinned)));
        for (i, (name, slot)) in [("ready", ready), ("building", building_slot(2))]
            .into_iter()
            .enumerate()
        {
            b.stats_cache
                .lock()
                .unwrap()
                .insert(scope.clone(), (foreign, slot));
            insert_one(&mut b, i);
            assert!(
                b.stats_cache.lock().unwrap().get(&scope).is_none(),
                "{name}: a foreign-version entry must not be merged into"
            );
            assert_eq!(b.memo_len(), 1, "{name}: the snapshot memo still merges");
        }
    }

    /// The first query on a scope plans on `ZeroStats` and starts the
    /// background build; the summary lands on its own, with one build.
    #[test]
    fn first_query_builds_stats_in_background() {
        let rebuilds = || horndb_metrics::metrics().sparql.stats_rebuild.get();
        let b = two_thousand_rows();
        let base = rebuilds();
        let _ = crate::api::execute_query(
            "SELECT ?s WHERE { ?s <http://ex/p> ?o . ?o <http://ex/p> ?x }",
            &b,
        )
        .unwrap();
        let landed = || {
            matches!(
                b.stats_cache
                    .lock()
                    .unwrap()
                    .get(&SnapshotScope::DefaultUnion),
                Some((_, StatsSlot::Ready(_)))
            )
        };
        for _ in 0..500 {
            if landed() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(landed(), "the horndb-stats thread must land the summary");
        assert_eq!(rebuilds(), base + 1);
    }

    /// Mirrors `tests/incremental_snapshot_delta.rs`'s
    /// `update_then_query_matches_fresh_backend_insert_data`. That test can
    /// only prove behavioural equivalence; `memo_len`/`Arc::as_ptr` are
    /// test-only and not reachable from an integration test in `tests/`, so
    /// the claim that the merge path (not a silent fallback) actually fired
    /// is checked here instead.
    #[test]
    fn update_then_query_matches_fresh_backend_insert_data_merges_in_place() {
        // Two seed rows, not one: the fast path only fires when the delta is
        // at most `base_rows / SNAPSHOT_DELTA_REBUILD_DIVISOR` (== 2 here), so
        // a single-row base would force the correct-but-uninteresting
        // fallback and this test would prove nothing about the merge path.
        let mut b = HornBackend::new();
        for (s, p, o) in [
            ("http://ex/a", "http://ex/p", "http://ex/b"),
            ("http://ex/other", "http://ex/p", "http://ex/o"),
        ] {
            b.insert_triple(
                Term::Iri(s.into()),
                Term::Iri(p.into()),
                Term::Iri(o.into()),
            );
        }
        assert_update_merges_in_place(
            &mut b,
            "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/c> }",
        );

        let mut fresh = HornBackend::new();
        for (s, p, o) in [
            ("http://ex/a", "http://ex/p", "http://ex/b"),
            ("http://ex/other", "http://ex/p", "http://ex/o"),
            ("http://ex/a", "http://ex/p", "http://ex/c"),
        ] {
            fresh.insert_triple(
                Term::Iri(s.into()),
                Term::Iri(p.into()),
                Term::Iri(o.into()),
            );
        }
        assert_eq!(all_spo(&b), all_spo(&fresh));
    }

    /// Mirrors `update_then_query_matches_fresh_backend_delete_data`. See
    /// `update_then_query_matches_fresh_backend_insert_data_merges_in_place`.
    #[test]
    fn update_then_query_matches_fresh_backend_delete_data_merges_in_place() {
        let mut b = HornBackend::new();
        for (s, p, o) in [
            ("http://ex/a", "http://ex/p", "http://ex/b"),
            ("http://ex/a", "http://ex/p", "http://ex/c"),
        ] {
            b.insert_triple(
                Term::Iri(s.into()),
                Term::Iri(p.into()),
                Term::Iri(o.into()),
            );
        }
        assert_update_merges_in_place(
            &mut b,
            "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        );

        let mut fresh = HornBackend::new();
        fresh.insert_triple(
            Term::Iri("http://ex/a".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/c".into()),
        );
        assert_eq!(all_spo(&b), all_spo(&fresh));
    }

    /// Mirrors `update_then_query_matches_fresh_backend_delete_absent_triple`.
    /// The delete targets a triple the store never held: the delta resolves
    /// to empty, and the merge path must still fire (not error, not drop the
    /// cache). See `update_then_query_matches_fresh_backend_insert_data_merges_in_place`.
    #[test]
    fn update_then_query_matches_fresh_backend_delete_absent_triple_merges_in_place() {
        let mut b = HornBackend::new();
        b.insert_triple(
            Term::Iri("http://ex/a".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/b".into()),
        );
        assert_update_merges_in_place(
            &mut b,
            "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/nope> }",
        );

        let mut fresh = HornBackend::new();
        fresh.insert_triple(
            Term::Iri("http://ex/a".into()),
            Term::Iri("http://ex/p".into()),
            Term::Iri("http://ex/b".into()),
        );
        assert_eq!(all_spo(&b), all_spo(&fresh));
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
    }

    #[test]
    fn clear_graph_all_graphs_sweeps_a_store_with_no_funnel_writes() {
        let mut b = HornBackend::new();
        // Plant a named-graph quad directly at the storage layer, bypassing
        // HornBackend's write funnel entirely. `clear_graph` must find it by
        // scanning the snapshot: this is the case an early-out based on
        // backend-side bookkeeping would skip, leaving the quad live (#265).
        // HDB-89 removed the `live_keys` mirror that made that mistake
        // possible; the test stays as the regression guard for the shape of
        // it, since the store is now the only place membership is recorded.
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
        assert_eq!(b.len(), 1);

        let removed = b.clear_graph(&GraphTarget::AllGraphs).unwrap();

        assert_eq!(removed, 1);
        assert!(b.is_empty());
        assert!(b.store.snapshot().graphs().is_empty());
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

#[cfg(test)]
mod reasoning_seam_tests {
    use super::*;
    use crate::algebra::Term as ATerm;
    use crate::exec::Executor;

    fn iri(v: &str) -> OxTerm {
        OxTerm::NamedNode(NamedNode::new_unchecked(v))
    }

    /// `oxrdf_to_lexical` is the exact inverse of `lexical_to_oxrdf` for
    /// every term kind the OWL 2 RL engine accepts. A view derivation reads
    /// source quads through the first and writes derived ones back through
    /// the second, so a term that does not survive the round trip is a
    /// derived triple written against a subtly different subject.
    #[test]
    fn lexical_round_trips_every_term_kind() {
        let terms = [
            iri("http://ex/s"),
            OxTerm::BlankNode(BlankNode::new_unchecked("b0")),
            OxTerm::Literal(Literal::new_simple_literal("plain")),
            OxTerm::Literal(Literal::new_language_tagged_literal("hi", "en").unwrap()),
            OxTerm::Literal(Literal::new_typed_literal(
                "42",
                NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"),
            )),
            OxTerm::Literal(Literal::new_simple_literal("has \"quotes\" and \\ slash")),
        ];
        for t in terms {
            let key = oxrdf_to_lexical(&t).expect("a supported term kind");
            assert_eq!(lexical_to_oxrdf(&key), t, "round trip failed for {key}");
        }
    }

    /// SPEC-29 D7: the write funnel reports which graphs actually changed,
    /// and a replayed identical batch reports nothing — that is what makes a
    /// re-applied change-feed batch derive zero.
    #[test]
    fn touched_graphs_report_only_real_changes() {
        let mut b = HornBackend::new();
        assert!(b.take_touched_graphs().is_empty(), "nothing written yet");

        let quad = |g: &str| {
            (
                Some(g.to_string()),
                ATerm::Iri("http://ex/s".into()),
                ATerm::Iri("http://ex/p".into()),
                ATerm::Iri("http://ex/o".into()),
            )
        };
        b.apply_quads(Vec::new(), vec![quad("http://ex/g1")])
            .unwrap();
        assert_eq!(
            b.take_touched_graphs(),
            vec![Some("http://ex/g1".to_string())]
        );
        assert!(b.take_touched_graphs().is_empty(), "the set drains");

        // Re-applying the identical batch changes nothing (SPEC-28 S6).
        let counts = b
            .apply_quads(Vec::new(), vec![quad("http://ex/g1")])
            .unwrap();
        assert_eq!((counts.inserted, counts.retracted), (0, 0));
        assert!(
            b.take_touched_graphs().is_empty(),
            "an idempotent no-op must not dirty a view"
        );

        // `CLEAR` sweeps one tier level down and still reports.
        b.clear_graph(&spargebra::algebra::GraphTarget::NamedNode(
            NamedNode::new_unchecked("http://ex/g1"),
        ))
        .unwrap();
        assert_eq!(
            b.take_touched_graphs(),
            vec![Some("http://ex/g1".to_string())]
        );
    }

    /// SPEC-29 D6: a reserved graph is out of the no-dataset default union
    /// and out of `GRAPH ?g` enumeration until it is opted back in by IRI —
    /// and opting one in must not drag the rest of the namespace along.
    #[test]
    fn visible_inferred_opts_in_exactly_the_named_reserved_graphs() {
        let mut b = HornBackend::new();
        let inferred = "https://horndb.io/graph/inferred/x";
        let catalog = "https://horndb.io/graph/views";
        // A distinct subject per graph, so the union's row count names the
        // graphs in it (a shared triple would be deduped by the snapshot
        // builder one layer down and hide the difference).
        for (i, g) in [inferred, catalog, "http://ex/data"].iter().enumerate() {
            b.insert_oxrdf_in_named_graph(
                &iri(g),
                &iri(&format!("http://ex/s{i}")),
                &iri("http://ex/p"),
                &iri("http://ex/o"),
            )
            .unwrap();
        }

        let enumerated = |b: &HornBackend| -> Vec<String> {
            b.named_graphs(None)
                .unwrap()
                .into_iter()
                .map(|n| n.iri)
                .collect()
        };
        assert_eq!(enumerated(&b), vec!["http://ex/data".to_string()]);
        assert_eq!(
            b.scope_triples(&SnapshotScope::DefaultUnion).len(),
            1,
            "only the non-reserved graph is in the union"
        );

        b.set_visible_inferred(BTreeSet::from([inferred.to_string()]));
        assert_eq!(
            enumerated(&b),
            vec!["http://ex/data".to_string(), inferred.to_string()],
            "the opted-in graph enumerates; the catalog graph does not"
        );
        assert_eq!(
            b.scope_triples(&SnapshotScope::DefaultUnion).len(),
            2,
            "the opted-in inferred graph joined the union; the catalog did not"
        );

        b.set_visible_inferred(BTreeSet::new());
        assert_eq!(enumerated(&b), vec!["http://ex/data".to_string()]);
    }
}
