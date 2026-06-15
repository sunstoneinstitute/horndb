//! Storage trait the generated rule code consumes.
//!
//! For Stage 1, the only impl shipped is `MemStore`. SPEC-02 will provide a
//! production backend implementing the same trait.

use crate::provenance::Provenance;
use crate::types::{MaxCardRestriction, TermId, Triple};
use crate::vocab::Vocabulary;
use rustc_hash::{FxHashMap, FxHashSet};

/// Iterator alias the trait returns. Boxed for object safety; Stage 2 can
/// specialise via a separate non-dyn trait if profiling demands it.
pub type TripleIter<'a> = Box<dyn Iterator<Item = Triple> + 'a>;

pub trait TripleStore {
    /// Vocabulary IDs (RDF/RDFS/OWL terms). Generated rule code calls this
    /// once on entry to avoid repeated lookups.
    fn vocab(&self) -> &Vocabulary;

    /// Does the store contain `t` (as asserted OR previously inferred)?
    fn contains(&self, t: &Triple) -> bool;

    /// Iterate all triples whose predicate equals `p`.
    fn scan_predicate(&self, p: TermId) -> TripleIter<'_>;

    /// Probe: return triples matching the given (subject?, predicate, object?) pattern.
    /// `None` slots are wildcards.
    fn probe(&self, s: Option<TermId>, p: TermId, o: Option<TermId>) -> TripleIter<'_>;

    /// Probe with a wildcard predicate (equivalent to scanning every
    /// predicate partition and filtering on (s?, o?)). Used by the
    /// `eq-rep-s` / `eq-rep-o` rules whose second pattern reads `?s ?p ?o`
    /// with the predicate not yet bound. Stage-1 cost is O(triples); a
    /// Stage-2 implementation can specialise (see TASKS.md).
    fn probe_any_predicate(&self, s: Option<TermId>, o: Option<TermId>) -> TripleIter<'_>;

    /// Insert an inferred triple with its proof. Returns true iff fresh.
    fn insert_inferred(&mut self, t: Triple, prov: Provenance) -> bool;

    /// Drop all inferred triples (asserted ones stay). Used by reset_and_materialize.
    fn clear_inferred(&mut self);

    /// All triples currently in the store, asserted + inferred. Stage 1 only.
    fn all_triples(&self) -> FxHashSet<Triple>;

    /// Resolved unqualified max-cardinality restrictions (`cls-maxc1`/`cls-maxc2`).
    /// Populated at load time by the embedder (`integration.rs`); empty for
    /// stores built directly without restriction resolution.
    fn card_restrictions(&self) -> &[MaxCardRestriction] {
        &[]
    }
}

/// Simple in-memory store keyed by predicate. Used by tests and by the
/// `RuleFiringBackend` reference implementation.
pub struct MemStore {
    vocab: Vocabulary,
    /// predicate → set of (subject, object)
    by_pred: FxHashMap<TermId, FxHashSet<(TermId, TermId)>>,
    /// proofs for inferred triples (asserted triples have no entry)
    proofs: FxHashMap<Triple, Provenance>,
    /// inferred set (subset of by_pred entries)
    inferred: FxHashSet<Triple>,
    /// Resolved max-cardinality restrictions (see `TripleStore::card_restrictions`).
    card_restrictions: Vec<MaxCardRestriction>,
}

impl MemStore {
    pub fn new(vocab: Vocabulary) -> Self {
        Self {
            vocab,
            by_pred: FxHashMap::default(),
            proofs: FxHashMap::default(),
            inferred: FxHashSet::default(),
            card_restrictions: Vec::new(),
        }
    }

    /// Insert an asserted (base) triple. Returns true iff fresh.
    pub fn assert(&mut self, t: Triple) -> bool {
        self.by_pred.entry(t.p).or_default().insert((t.s, t.o))
    }

    pub fn assert_all<I: IntoIterator<Item = Triple>>(&mut self, ts: I) {
        for t in ts {
            self.assert(t);
        }
    }

    /// True iff `t` was added via `insert_inferred` (not via `assert`).
    pub fn is_inferred(&self, t: &Triple) -> bool {
        self.inferred.contains(t)
    }

    pub fn proof(&self, t: &Triple) -> Option<&Provenance> {
        self.proofs.get(t)
    }

    /// Set the resolved max-cardinality restrictions. Called once at load
    /// time (`integration.rs`) or directly by tests.
    pub fn set_card_restrictions(&mut self, restrictions: Vec<MaxCardRestriction>) {
        self.card_restrictions = restrictions;
    }
}

impl TripleStore for MemStore {
    fn vocab(&self) -> &Vocabulary {
        &self.vocab
    }

    fn contains(&self, t: &Triple) -> bool {
        self.by_pred
            .get(&t.p)
            .is_some_and(|set| set.contains(&(t.s, t.o)))
    }

    fn scan_predicate(&self, p: TermId) -> TripleIter<'_> {
        match self.by_pred.get(&p) {
            Some(set) => Box::new(set.iter().map(move |&(s, o)| Triple::new(s, p, o))),
            None => Box::new(std::iter::empty()),
        }
    }

    fn probe(&self, s: Option<TermId>, p: TermId, o: Option<TermId>) -> TripleIter<'_> {
        match self.by_pred.get(&p) {
            Some(set) => {
                let iter = set.iter().filter_map(move |&(ss, oo)| {
                    if s.is_none_or(|x| x == ss) && o.is_none_or(|x| x == oo) {
                        Some(Triple::new(ss, p, oo))
                    } else {
                        None
                    }
                });
                Box::new(iter)
            }
            None => Box::new(std::iter::empty()),
        }
    }

    fn probe_any_predicate(&self, s: Option<TermId>, o: Option<TermId>) -> TripleIter<'_> {
        let iter = self.by_pred.iter().flat_map(move |(&p, set)| {
            set.iter().filter_map(move |&(ss, oo)| {
                if s.is_none_or(|x| x == ss) && o.is_none_or(|x| x == oo) {
                    Some(Triple::new(ss, p, oo))
                } else {
                    None
                }
            })
        });
        Box::new(iter)
    }

    fn insert_inferred(&mut self, t: Triple, prov: Provenance) -> bool {
        let fresh = self.by_pred.entry(t.p).or_default().insert((t.s, t.o));
        if fresh {
            self.inferred.insert(t);
            self.proofs.insert(t, prov);
        }
        fresh
    }

    fn clear_inferred(&mut self) {
        let to_remove: Vec<Triple> = self.inferred.iter().copied().collect();
        self.inferred.clear();
        for t in to_remove {
            if let Some(set) = self.by_pred.get_mut(&t.p) {
                set.remove(&(t.s, t.o));
                if set.is_empty() {
                    self.by_pred.remove(&t.p);
                }
            }
        }
        self.proofs.clear();
    }

    fn all_triples(&self) -> FxHashSet<Triple> {
        let mut out = FxHashSet::default();
        for (&p, set) in &self.by_pred {
            for &(s, o) in set {
                out.insert(Triple::new(s, p, o));
            }
        }
        out
    }

    fn card_restrictions(&self) -> &[MaxCardRestriction] {
        &self.card_restrictions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn store() -> MemStore {
        MemStore::new(Vocabulary::synthetic(1000))
    }

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn assert_and_contains() {
        let mut s = store();
        assert!(s.assert(t(1, 2, 3)));
        assert!(!s.assert(t(1, 2, 3))); // dedup
        assert!(s.contains(&t(1, 2, 3)));
        assert!(!s.contains(&t(1, 2, 4)));
    }

    #[test]
    fn scan_predicate_returns_all_matches() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.assert(t(4, 2, 5));
        s.assert(t(6, 7, 8));
        let got: Vec<_> = s.scan_predicate(TermId(2)).collect();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn probe_filters_subject_and_object() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.assert(t(1, 2, 4));
        s.assert(t(5, 2, 3));
        let got: Vec<_> = s.probe(Some(TermId(1)), TermId(2), None).collect();
        assert_eq!(got.len(), 2);
        let got: Vec<_> = s.probe(None, TermId(2), Some(TermId(3))).collect();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn card_restrictions_round_trip() {
        use crate::types::MaxCardRestriction;
        let mut s = store();
        assert!(s.card_restrictions().is_empty());
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            max: 1,
        }]);
        assert_eq!(s.card_restrictions().len(), 1);
        assert_eq!(s.card_restrictions()[0].max, 1);
    }

    #[test]
    fn clear_inferred_keeps_asserted() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.insert_inferred(
            t(4, 5, 6),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        assert!(s.contains(&t(4, 5, 6)));
        s.clear_inferred();
        assert!(s.contains(&t(1, 2, 3)));
        assert!(!s.contains(&t(4, 5, 6)));
    }
}
