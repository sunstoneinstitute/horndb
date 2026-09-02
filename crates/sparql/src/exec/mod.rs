//! Executor seam: SPARQL planner -> storage/join backend.
//!
//! Stage 1 ships a single in-crate implementation [`mem::MemStore`]
//! over a `HashSet<(s,p,o)>`. SPEC-03 (WCOJ engine) will provide a
//! production implementation through the same trait.

pub mod batch;
pub use batch::{Batch, KeyPart, Row, Slot};
pub mod horn;
pub mod mem;
pub mod op;
pub(crate) mod phases;
pub mod runtime;
pub mod scope;
pub use scope::{
    is_reserved_graph, per_graph_needs_the_scan_loop, NamedGraph, ResolvedScope, ScanScope,
    RESERVED_GRAPH_PREFIX,
};

use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use std::collections::BTreeMap;

/// A single SPARQL solution mapping.
///
/// We use `BTreeMap` so the order of variables in serialised results
/// is deterministic for snapshot tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Bindings {
    inner: BTreeMap<String, Term>,
}

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, var: &str) -> Option<&Term> {
        self.inner.get(var)
    }
    pub fn set(&mut self, var: impl Into<String>, term: Term) {
        self.inner.insert(var.into(), term);
    }
    pub fn vars(&self) -> impl Iterator<Item = (&str, &Term)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn extend_compat(&self, other: &Bindings) -> Option<Bindings> {
        // Compatible: every shared var has the same term. Merge wins.
        let mut out = self.clone();
        for (k, v) in &other.inner {
            match out.inner.get(k) {
                Some(existing) if existing != v => return None,
                _ => {
                    out.inner.insert(k.clone(), v.clone());
                }
            }
        }
        Some(out)
    }
    /// Return the set of variables bound in this row.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.inner.keys().map(|s| s.as_str())
    }
    /// Number of bound variables. Useful in tests and slicing.
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// One group's key slots and its row count, as produced by
/// [`Executor::count_bgp_grouped`].
pub type GroupCount = (Vec<Slot>, usize);

/// The single seam Stage 1 needs from the storage/join backend.
/// SPEC-03 will eventually back this with Leapfrog Triejoin; in the
/// meantime [`mem::MemStore`] satisfies it for tests.
///
/// Every read takes a [`ScanScope`]: the graph(s) the scan reads, resolved
/// from the plan leaf's `GRAPH` scope plus the query's dataset clause and
/// `default_graph` mode (SPEC-28 S3). A backend that cannot express a scope
/// must **error or decline**, never widen it — a whole-store answer under a
/// graph scope is exactly the silent wrong answer SPEC-28 exists to remove.
pub trait Executor {
    /// Iterate solutions to a BGP within `scope`. Implementations are free
    /// to optimise — `MemStore` uses a naive nested loop.
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
        scope: &ScanScope<'_>,
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>>;

    /// Scan a BGP returning id-carrying slot rows (no `TermId → String`
    /// decode). The default adapts the string [`scan_bgp`] for backends
    /// without a dictionary (e.g. `MemStore`, test doubles): the rows come
    /// back as `Slot::Term`. `HornBackend` overrides this to read the WCOJ
    /// id columns directly.
    // keep in sync with HornBackend::scan_bgp_ids
    fn scan_bgp_ids(&self, patterns: &[TriplePattern], scope: &ScanScope<'_>) -> Result<Batch> {
        let rows: Vec<Bindings> = self.scan_bgp(patterns, scope)?.collect();
        Ok(Batch::from_bindings(rows))
    }

    /// The graphs `GRAPH ?g` enumerates, in a deterministic order (SPEC-28
    /// S3/D6). The scan operator calls this once per `GRAPH ?g` leaf, then
    /// scans each graph on its own — which is why this returns graph *names*
    /// and never a widened scope.
    ///
    /// `named` is the query's `FROM NAMED` set as
    /// [`ResolvedScope::PerGraph`] carries it: `None` = every non-reserved
    /// graph the backend holds; `Some(list)` = exactly those of `list` the
    /// backend holds, reserved ones included (naming one is the opt-in).
    /// The default graph is never in the result (D3), and a name matching
    /// no graph simply contributes nothing — never an error.
    ///
    /// The default implementation refuses: a backend that cannot enumerate
    /// its graphs must not let `GRAPH ?g` answer over the wrong ones.
    fn named_graphs(&self, _named: Option<&[String]>) -> Result<Vec<NamedGraph>> {
        Err(crate::error::SparqlError::UnsupportedAlgebra(
            "GRAPH ?g: this backend cannot enumerate named graphs".into(),
        ))
    }

    /// Decode a dictionary id to its term. Only meaningful for backends that
    /// produce `Slot::Id` (i.e. `HornBackend`); the default errors and is
    /// never reached for backends whose `scan_bgp_ids` yields only
    /// `Slot::Term`.
    fn decode_term(&self, id: horndb_storage::TermId) -> Result<Term> {
        Err(crate::error::SparqlError::Executor(format!(
            "backend has no dictionary to decode {id:?}"
        )))
    }

    /// Look up a term's dictionary id without interning (the inverse of
    /// [`decode_term`]). `None` means the backend has no dictionary, or the
    /// term is simply not stored (so no `Slot::Id` can carry its value).
    ///
    /// Used to canonicalize hash-join keys: a `Slot::Term(t)` and a
    /// `Slot::Id(i)` that name the *same* value must land in the same bucket.
    /// Encoding the term back to its id when present makes the key
    /// provenance-independent while paying zero decode on the common all-`Id`
    /// path. The default returns `None`, so dictionary-less backends
    /// (`MemStore`, whose rows are all `Slot::Term`) fall back to lexical keys
    /// on both sides — consistent, just not id-compressed.
    fn encode_term(&self, _term: &Term) -> Option<horndb_storage::TermId> {
        None
    }

    /// Read a term's numeric value directly, skipping the `Term` clone +
    /// N-Triples round trip [`decode_term`](Executor::decode_term) followed
    /// by the SPARQL-side numeric coercion would otherwise pay per row
    /// (HDB-100): the `SUM`/`AVG`/`MIN`/`MAX` fast paths in
    /// `exec/runtime.rs::eval_group_native` fold this way when their inner
    /// expression is a bare scan-column variable. `None` for a non-literal
    /// term, an id the backend cannot resolve, or a literal whose value does
    /// not parse as `f64` — the same cases
    /// `runtime::numeric_value(&self.decode_term(id)?)` would treat as "not a
    /// number", which is exactly the default below. `HornBackend` overrides
    /// this to read the dictionary's stored `oxrdf::Literal` value in place,
    /// under one lock, with no clone/`to_string`/unescape/re-parse.
    fn decode_numeric(&self, id: horndb_storage::TermId) -> Result<Option<f64>> {
        Ok(crate::exec::runtime::numeric_value(&self.decode_term(id)?))
    }

    /// Batched [`decode_term`](Executor::decode_term): decode every id in
    /// `ids`, in order. The default calls `decode_term` once per id
    /// (correct, just not batched); `HornBackend` overrides this to take the
    /// dictionary's read lock once for the whole batch
    /// (`Dictionary::lookup_batch`) rather than once per id (HDB-100).
    fn decode_terms(&self, ids: &[horndb_storage::TermId]) -> Result<Vec<Term>> {
        ids.iter().map(|&id| self.decode_term(id)).collect()
    }

    /// Best-effort estimate of how many solution rows a BGP yields,
    /// used by `EXPLAIN` (SPEC-07 F9) for per-node cardinality
    /// annotations. The default returns `None` ("unknown"); backends
    /// that can cheaply count (e.g. an in-memory triple set) should
    /// override it. The number is an *estimate*, not a guarantee —
    /// `EXPLAIN` labels it with `~`.
    ///
    /// This deliberately does not execute the BGP join: a leaf-pattern
    /// row count is enough for the Stage-1 plan printer, which has no
    /// cost model.
    /// `scope` is advisory here: an estimate that ignores it stays a valid
    /// upper bound, and SPEC-28 S3 permits coarse estimates precisely
    /// because they never reach a result.
    fn cardinality_estimate(
        &self,
        _patterns: &[TriplePattern],
        _scope: &ScanScope<'_>,
    ) -> Option<usize> {
        None
    }

    /// Count solutions to a BGP without materializing rows. `None` = "no fast
    /// count available" (caller falls back to scanning). Additive; does not
    /// change `scan_bgp_ids`. The returned count, when `Some`, MUST equal the
    /// number of solution rows `scan_bgp_ids` would produce (one row per BGP
    /// solution) **in `scope`**. A backend that cannot count within a given
    /// scope must return `None` (or error), never a wider count — the
    /// caller's scan fallback is scope-correct by construction.
    fn count_bgp(
        &self,
        _patterns: &[TriplePattern],
        _scope: &ScanScope<'_>,
    ) -> Result<Option<usize>> {
        Ok(None)
    }

    /// Per-group solution counts for a BGP grouped by `keys`, without
    /// materializing rows. `None` = "no fast grouped count available" (the
    /// caller falls back to scanning + hash-counting the key columns).
    /// Additive; does not change `scan_bgp_ids`. When `Some`, the groups
    /// MUST partition the rows `scan_bgp_ids` would produce, keyed by term
    /// identity of the key columns: each entry carries one group's key slots
    /// (scan provenance preserved) and its row count, in no particular order.
    fn count_bgp_grouped(
        &self,
        _patterns: &[TriplePattern],
        _keys: &[Var],
        _scope: &ScanScope<'_>,
    ) -> Result<Option<Vec<GroupCount>>> {
        Ok(None)
    }
}

/// A quad's graph slot in the write trait's lexical form: `None` is the
/// default-graph sentinel (SPARQL's unnamed graph), `Some(iri)` a named
/// graph's IRI. Promoted from `mem.rs`'s pre-existing internal convention
/// (SPEC-28 phase 3) so [`Store`] and both backends share one name for it.
pub type GraphName = Option<String>;

/// A ground `(subject, predicate, object)` triple in algebra-[`Term`] form —
/// what [`Store::scan_graph_quads`] reads back.
pub type AlgebraTriple = (Term, Term, Term);

/// A ground `(graph, subject, predicate, object)` quad in algebra-[`Term`]
/// form — the unit [`Store::apply_quads`] applies. `graph = None` is the
/// default graph.
pub type AlgebraQuad = (GraphName, Term, Term, Term);

/// How many quads [`Store::apply_quads`] actually changed, after collapsing
/// no-ops: deleting an already-absent quad, or inserting an already-visible
/// one, adds to neither field (SPEC-28 S6). Mirrors
/// `horndb_storage::tier::ApplyReport`'s shape one layer up the stack, so a
/// backend with no dependency on the storage crate (`MemStore`) can still
/// report it in the same shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ApplyCounts {
    pub retracted: usize,
    pub inserted: usize,
}

/// A storage-side write seam used by [`crate::update`] (SPARQL Update,
/// SPEC-28 phase 4) — quad-shaped, so a write can target any graph, not just
/// the default one.
///
/// `Store` is intentionally separate from `Executor` so that read-only
/// backends (e.g. mmap'd HDT) can implement only the read side.
pub trait Store {
    /// Apply one atomic batch of deletions and insertions (SPEC-28 S6):
    /// `dels` take effect before `adds`, so a quad deleted and re-added
    /// within the same call ends up present. Idempotent and counted — a
    /// deletion that was already absent, or an insertion that was already
    /// visible, changes nothing and does not add to the returned
    /// [`ApplyCounts`].
    fn apply_quads(
        &mut self,
        dels: Vec<AlgebraQuad>,
        adds: Vec<AlgebraQuad>,
    ) -> Result<ApplyCounts>;

    /// Remove every quad `graph` selects — the `CLEAR`/`DROP` sweep, and the
    /// destination-clearing step of `COPY`/`MOVE`. Implemented via
    /// [`Store::apply_quads`] internally (a pure deletion batch), so it is
    /// idempotent and counted the same way. Returns the number of quads
    /// actually retracted.
    fn clear_graph(&mut self, graph: &spargebra::algebra::GraphTarget) -> Result<usize>;

    /// True if the named graph `graph` (an IRI) currently holds at least one
    /// visible quad — SPEC-28 D11's existence rule: a named graph exists iff
    /// it holds a quad, so a fully-cleared graph stops existing rather than
    /// lingering as an empty entry. The default graph has no IRI and is
    /// never `graph`'s value.
    fn graph_exists(&self, graph: &str) -> bool;

    /// True if this exact quad is currently visible — a point read of the
    /// same state [`Store::apply_quads`] would find. Backs the
    /// multi-operation Update rollback journal (`crate::update`), which reads
    /// it once per quad an operation is about to touch so a failing later
    /// operation can restore the pre-request state.
    ///
    /// The default answers it by scanning the quad's whole graph. A backend
    /// that can answer it as a point read should override it — `MemStore` and
    /// `HornBackend` both do.
    fn quad_exists(&self, quad: &AlgebraQuad) -> bool {
        let (g, s, p, o) = quad;
        let target = match g {
            None => spargebra::algebra::GraphTarget::DefaultGraph,
            Some(iri) => spargebra::algebra::GraphTarget::NamedNode(
                spargebra::term::NamedNode::new_unchecked(iri),
            ),
        };
        match self.scan_graph_quads(&target) {
            Ok(triples) => triples
                .iter()
                .any(|(ts, tp, to)| ts == s && tp == p && to == o),
            Err(_) => false,
        }
    }

    /// Every named graph currently holding at least one visible quad, by
    /// IRI. Backs `DROP ALL`'s per-graph sweep and `ADD`/`MOVE`/`COPY`'s
    /// graph enumeration.
    ///
    /// Named `graphs`, not `named_graphs`: a same-named method on
    /// [`Executor`] already answers a different, query-side question (`GRAPH
    /// ?g` enumeration, filtered by `FROM NAMED`/the reserved-graph prefix,
    /// returning [`NamedGraph`]s with a query-binding `Slot`) — and Rust's
    /// trait-method resolution treats two same-named methods on different
    /// traits implemented for the same type as ambiguous regardless of
    /// differing arity, so the two names must differ.
    fn graphs(&self) -> Vec<String>;

    /// Every triple currently visible in `graph` — the `ADD`/`MOVE`/`COPY`
    /// source read. `graph` naming `AllGraphs`/`NamedGraphs` (not a single
    /// graph) is a caller error: there is no one source to read.
    fn scan_graph_quads(
        &self,
        graph: &spargebra::algebra::GraphTarget,
    ) -> Result<Vec<AlgebraTriple>>;

    /// Insert one triple into the default graph. A back-compat convenience
    /// wrapper over [`Store::apply_quads`], kept for the pre-SPEC-28-phase-4
    /// test suite's ~180 call sites that seed default-graph fixtures;
    /// production writes (`crate::update`) call `apply_quads` directly so
    /// they can target any graph. Silently drops a term `apply_quads`
    /// cannot represent (a variable, or an RDF 1.2 triple term), matching
    /// this method's pre-recut behaviour.
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        let _ = self.apply_quads(Vec::new(), vec![(None, subject, predicate, object)]);
    }

    /// Delete one triple from the default graph. See
    /// [`Store::insert_triple`] for why this stays a default method rather
    /// than a call-site rewrite.
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        let _ = self.apply_quads(
            vec![(None, subject.clone(), predicate.clone(), object.clone())],
            Vec::new(),
        );
    }

    /// A fresh tag for one document `LOAD`s into this store (HDB-113): blank
    /// node labels are document-scoped in N-Triples/Turtle/N-Quads, so
    /// `update.rs::fetch_and_parse` renames every one it parses with
    /// `horndb_storage::loader::scope_blank_node(tag, ...)`, tagged by this,
    /// before turning it into an algebra `Term`. Same rename the bulk loaders
    /// apply via `horndb_storage::Store::next_bnode_doc_tag`.
    ///
    /// Default: a process-wide counter, for a backend with no
    /// `horndb_storage::Store` of its own to scope it to (e.g. `MemStore`).
    /// [`HornBackend`](crate::exec::horn::HornBackend) overrides this to
    /// delegate to its store's own counter.
    fn next_bnode_doc_tag(&self) -> u64 {
        static GLOBAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        GLOBAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

/// Convenience: a backend that is both an `Executor` and a `Store`.
pub trait FullBackend: Executor + Store {}
impl<T: Executor + Store> FullBackend for T {}

/// Classify a stored lexical value back into the term kind it encodes.
///
/// The Stage-1 store keeps triples as `(String, String, String)` in
/// N-Triples lexical form, which loses the term's syntactic kind. We
/// recover the kind from the lexical shape so a bound value surfaces as
/// the right `Term` variant (IRI vs literal vs blank node) — enough for
/// correct SPARQL-XML element types and value-aware ORDER BY without
/// widening the storage representation. SPEC-02's dictionary store will
/// carry the kind explicitly and make this unnecessary.
///
/// Rules (N-Triples object lexical forms):
///   * starts with `"` → a literal (`"v"`, `"v"@lang`, `"v"^^<dt>`);
///   * otherwise → an IRI.
///
/// Scope note (rung 4): this recovers only the IRI-vs-literal
/// distinction, which is what the SPARQL-XML element type and
/// value-aware comparison need. Blank nodes are stored as bare labels
/// (oxrdf's `BlankNode::as_str()` drops the `_:`), so they are
/// indistinguishable from IRIs at this lexical layer and remain
/// classified as IRIs — the same behaviour as before this change.
/// Faithful blank-node round-tripping is deferred to the dictionary
/// store (SPEC-02), which carries the kind explicitly.
pub(crate) fn classify_lexical(val: &str) -> Term {
    if val.starts_with('"') {
        Term::Literal(val.to_owned())
    } else {
        Term::Iri(val.to_owned())
    }
}

/// Helper used by the executor: bind a single pattern against a
/// concrete triple, returning the new bindings or `None` if the
/// constants don't match.
pub(crate) fn unify_one(
    pat: &TriplePattern,
    triple: &(String, String, String),
    prior: &Bindings,
) -> Option<Bindings> {
    let mut out = prior.clone();
    for (term, val) in [
        (&pat.subject, &triple.0),
        (&pat.predicate, &triple.1),
        (&pat.object, &triple.2),
    ] {
        match term {
            Term::Var(v) => {
                // Recover the term kind from the stored lexical form so a
                // bound literal surfaces as `Term::Literal`, not as an
                // IRI. Stored blank nodes carry their `_:` prefix.
                let new = classify_lexical(val);
                match out.get(v.name()) {
                    Some(existing) if existing != &new => return None,
                    _ => out.set(v.name().to_owned(), new),
                }
            }
            // A constant pattern term matches the stored lexical value.
            Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => {
                if s != val {
                    return None;
                }
            }
            // RDF 1.2 triple-term patterns reach this far only if a
            // caller bypasses the translator's SparqlConfig gate;
            // unify_one only deals with lexical-form (s, p, o) tuples
            // and has no way to recurse into a triple-term sub-pattern.
            Term::Triple(_) => return None,
        }
    }
    Some(out)
}
