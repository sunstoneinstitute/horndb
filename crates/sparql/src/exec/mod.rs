//! Executor seam: SPARQL planner -> storage/join backend.
//!
//! Stage 1 ships a single in-crate implementation [`mem::MemStore`]
//! over a `HashSet<(s,p,o)>`. SPEC-03 (WCOJ engine) will provide a
//! production implementation through the same trait.

pub mod batch;
pub use batch::{Batch, KeyPart, Row, Slot};
pub mod horn;
pub mod mem;
pub mod runtime;

use crate::algebra::{Term, TriplePattern};
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

/// The single seam Stage 1 needs from the storage/join backend.
/// SPEC-03 will eventually back this with Leapfrog Triejoin; in the
/// meantime [`mem::MemStore`] satisfies it for tests.
pub trait Executor {
    /// Iterate solutions to a BGP. Implementations are free to
    /// optimise — `MemStore` uses a naive nested loop.
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>>;

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
    fn cardinality_estimate(&self, _patterns: &[TriplePattern]) -> Option<usize> {
        None
    }
}

/// A storage-side write seam used by [`crate::update`].
///
/// `Store` is intentionally separate from `Executor` so that read-only
/// backends (e.g. mmap'd HDT) can implement only the read side.
pub trait Store {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term);
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term);
    /// Remove every triple from the (single, default) graph. Backs the
    /// graph-management `CLEAR`/`DROP` verbs and the destination-clearing
    /// step of `COPY`/`MOVE` under the Stage-1 default-graph-only model.
    fn clear_all(&mut self);
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
