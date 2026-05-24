//! Executor seam: SPARQL planner -> storage/join backend.
//!
//! Stage 1 ships a single in-crate implementation [`mem::MemStore`]
//! over a `HashSet<(s,p,o)>`. SPEC-03 (WCOJ engine) will provide a
//! production implementation through the same trait.

pub mod mem;
pub mod runtime;

use crate::algebra::{Term, TriplePattern};
use crate::error::Result;
use std::collections::BTreeMap;

/// A single SPARQL solution mapping.
///
/// We use `BTreeMap` so the order of variables in serialised results
/// is deterministic for snapshot tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
}

/// A storage-side write seam used by [`crate::update`].
///
/// `Store` is intentionally separate from `Executor` so that read-only
/// backends (e.g. mmap'd HDT) can implement only the read side.
pub trait Store {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term);
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term);
}

/// Convenience: a backend that is both an `Executor` and a `Store`.
pub trait FullBackend: Executor + Store {}
impl<T: Executor + Store> FullBackend for T {}

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
                let new = Term::Iri(val.clone());
                match out.get(v.name()) {
                    Some(existing) if existing != &new => return None,
                    _ => out.set(v.name().to_owned(), new),
                }
            }
            Term::Iri(s) => {
                if s != val {
                    return None;
                }
            }
            Term::Literal(s) => {
                if s != val {
                    return None;
                }
            }
            Term::BlankNode(s) => {
                if s != val {
                    return None;
                }
            }
        }
    }
    Some(out)
}
