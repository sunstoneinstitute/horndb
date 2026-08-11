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
pub mod runtime;
pub mod scope;
pub use scope::{
    is_reserved_graph, per_graph_needs_the_scan_loop, NamedGraph, ResolvedScope, ScanScope,
    RESERVED_GRAPH_PREFIX,
};

use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use spargebra::algebra::GraphTarget;
use std::collections::BTreeMap;

// The quad-shaped [`Store`] seam takes a [`GraphTarget`] (and callers build one
// from a [`NamedNode`]); both are spargebra types that appear in this crate's
// public API, so re-export them here so downstream code — and this crate's
// integration tests — can name them without a direct `spargebra` dependency.
pub use spargebra::algebra::GraphTarget as StoreGraphTarget;
pub use spargebra::term::NamedNode as GraphNamedNode;

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

/// One RDF quad in algebra-[`Term`] form: `None` names the default graph,
/// `Some(Term)` a named graph (the term is normally a [`Term::Iri`]). Mirrors
/// the pre-existing `(Term, Term, Term)` triple convention used throughout
/// `update.rs` — see [`AlgebraTriple`] — so callers build these directly from
/// parsed quad patterns/templates without a separate encoding.
pub type AlgebraQuad = (Option<Term>, Term, Term, Term);

/// One RDF triple in algebra-[`Term`] form: `(subject, predicate, object)`.
pub type AlgebraTriple = (Term, Term, Term);

/// The result of one [`Store::apply_quads`] batch.
///
/// Reuses [`horndb_storage::ApplyReport`] verbatim rather than re-deriving an
/// identically-shaped exec-layer struct: `HornBackend` returns storage's own
/// report unchanged, and `MemStore` computes the same two counts by the same
/// rule (SPEC-28 S6), so a separate type would carry nothing backend-specific.
pub type ApplyCounts = horndb_storage::ApplyReport;

/// A storage-side write seam used by [`crate::update`].
///
/// `Store` is intentionally separate from `Executor` so that read-only
/// backends (e.g. mmap'd HDT) can implement only the read side.
///
/// Quad-shaped (SPEC-28 S4/S6): every write names its graph explicitly, so a
/// backend with named-graph data has one seam for every write, not a
/// default-graph-only path plus ad hoc named-graph escape hatches. Phase 2
/// deliberately left this triple-shaped (see `HornBackend`'s history); this
/// phase (#267) re-cuts it.
pub trait Store {
    /// One atomic batch: `dels` take effect before `adds`, so a del+add of
    /// the same quad within one batch ends the batch with that quad present.
    /// Idempotent and counted (SPEC-28 S6): `retracted`/`inserted` count only
    /// quads whose visibility actually changed — retracting an absent quad,
    /// or inserting an already-visible one, changes neither count. Quad
    /// identity is per-graph: the same triple in two different graphs is two
    /// distinct quads.
    fn apply_quads(
        &mut self,
        dels: Vec<AlgebraQuad>,
        adds: Vec<AlgebraQuad>,
    ) -> Result<ApplyCounts>;

    /// `CLEAR`/`DROP` sweep of `graph`'s quads, implemented via
    /// [`Self::apply_quads`] internally — never a structural unlink — so the
    /// sweep is atomic and counted like any other write. Returns the number
    /// of quads retracted. `graph` selects the target the same way SPARQL
    /// Update's `CLEAR`/`DROP` grammar does: `DefaultGraph`, one
    /// `NamedNode`, every named graph (`NamedGraphs`), or the whole store
    /// (`AllGraphs`).
    fn clear_graph(&mut self, graph: &GraphTarget) -> Result<usize>;

    /// D11 existence: a graph exists if and only if it holds at least one
    /// visible quad. An emptied graph does not exist.
    fn graph_exists(&self, graph: &str) -> bool;

    /// Every named graph currently holding at least one visible quad, for
    /// `DROP ALL` / `ADD`/`MOVE`/`COPY` enumeration. Never includes the
    /// default graph.
    fn named_graphs(&self) -> Vec<String>;

    /// Every visible triple in `graph` — the source read for
    /// `ADD`/`MOVE`/`COPY`. `graph` is `DefaultGraph` or a single
    /// `NamedNode`; `NamedGraphs`/`AllGraphs` name more than one graph and
    /// have no single-triple-list reading, so implementations reject them.
    fn scan_graph_quads(&self, graph: &GraphTarget) -> Result<Vec<AlgebraTriple>>;
}

/// Convenience: a backend that is both an `Executor` and a `Store`.
pub trait FullBackend: Executor + Store {}
impl<T: Executor + Store> FullBackend for T {}

/// Test/fixture convenience layer over [`Store`], preserving the pre-SPEC-28
/// phase-4 single-triple call shape (`insert_triple`/`delete_triple`/
/// `clear_all`, all default-graph scoped).
///
/// The quad-shaped [`Store::apply_quads`]/[`Store::clear_graph`] are the real
/// write seam (used by `update.rs` and `crates/python`'s `RdfGraph::remove`).
/// This extension trait exists only because dozens of unrelated tests across
/// this crate (SELECT/CONSTRUCT/DESCRIBE/join/aggregate fixtures, mostly)
/// build a store with one-triple-at-a-time calls that have nothing to do
/// with named-graph semantics; rewriting every call site to build
/// `AlgebraQuad`s by hand would be pure churn. Blanket-implemented for every
/// `Store`, so a type need only implement `Store` to get this for free.
///
/// Gated behind `cfg(any(test, feature = "test-util"))`: this is fixture
/// sugar, not part of the production API. Left un-gated, `insert_triple`/
/// `delete_triple`/`clear_all` — hardcoding the default graph and swallowing
/// `apply_quads`'s `Result` — would be reachable from any production module
/// that imports it, defeating the graph-naming discipline the quad-shaped
/// `Store` re-cut exists to enforce. `cfg(test)` alone is not enough: the
/// integration tests under `crates/sparql/tests/*.rs` are a separate
/// compilation unit that links the *non*-test library build, so they need
/// the `test-util` feature — enabled automatically for this crate's own
/// test/example/bench targets via the self-dependency in `Cargo.toml`'s
/// `[dev-dependencies]`, with no `--features` change needed on the test
/// command.
#[cfg(any(test, feature = "test-util"))]
pub trait StoreTestExt: Store {
    /// Insert one triple into the default graph. Errors from `apply_quads`
    /// are swallowed (matches the old trait method's `()` return) — test
    /// fixtures pass ground terms that always intern cleanly.
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        let _ = self.apply_quads(Vec::new(), vec![(None, subject, predicate, object)]);
    }

    /// Delete one triple from the default graph (a no-op if absent, or if it
    /// exists only in a named graph).
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        let _ = self.apply_quads(
            vec![(None, subject.clone(), predicate.clone(), object.clone())],
            Vec::new(),
        );
    }

    /// Whole-store wipe (every graph, default and named) — the pre-#267
    /// `clear_all` behaviour, via `clear_graph(&GraphTarget::AllGraphs)`.
    fn clear_all(&mut self) {
        let _ = self.clear_graph(&GraphTarget::AllGraphs);
    }
}
#[cfg(any(test, feature = "test-util"))]
impl<T: Store + ?Sized> StoreTestExt for T {}

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
