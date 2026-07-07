//! Storage trait the generated rule code consumes.
//!
//! For Stage 1, the only impl shipped is `MemStore`. SPEC-02 will provide a
//! production backend implementing the same trait.

use crate::provenance::{ProofTree, Provenance};
use crate::types::{MaxCardRestriction, QualMaxCardRestriction, TermId, Triple};
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

    /// Resolved qualified max-cardinality restrictions (`cls-maxqc1`–`cls-maxqc4`).
    /// Populated at load time by the embedder (`integration.rs`); empty for
    /// stores built directly without restriction resolution.
    fn qual_card_restrictions(&self) -> &[crate::types::QualMaxCardRestriction] {
        &[]
    }
}

/// Simple in-memory store keyed by predicate. Used by tests and by the
/// `RuleFiringBackend` reference implementation.
pub struct MemStore {
    vocab: Vocabulary,
    /// predicate → set of (subject, object)
    by_pred: FxHashMap<TermId, FxHashSet<(TermId, TermId)>>,
    /// predicate → object → set of subjects. Mirrors `by_pred`; lets
    /// `probe(None, p, Some(o))` return O(|extent|) instead of O(|partition|).
    obj_index: FxHashMap<TermId, FxHashMap<TermId, FxHashSet<TermId>>>,
    /// proofs for inferred triples (asserted triples have no entry)
    proofs: FxHashMap<Triple, Provenance>,
    /// inferred set (subset of by_pred entries)
    inferred: FxHashSet<Triple>,
    /// Resolved max-cardinality restrictions (see `TripleStore::card_restrictions`).
    card_restrictions: Vec<MaxCardRestriction>,
    /// Resolved qualified max-cardinality restrictions (see `TripleStore::qual_card_restrictions`).
    qual_card_restrictions: Vec<QualMaxCardRestriction>,
}

impl MemStore {
    pub fn new(vocab: Vocabulary) -> Self {
        Self {
            vocab,
            by_pred: FxHashMap::default(),
            obj_index: FxHashMap::default(),
            proofs: FxHashMap::default(),
            inferred: FxHashSet::default(),
            card_restrictions: Vec::new(),
            qual_card_restrictions: Vec::new(),
        }
    }

    /// Add `t.s` to the object index bucket for (t.p, t.o).
    fn index_insert(&mut self, t: Triple) {
        self.obj_index
            .entry(t.p)
            .or_default()
            .entry(t.o)
            .or_default()
            .insert(t.s);
    }

    /// Remove `t.s` from the object index, pruning empty inner sets and empty
    /// predicate maps so no empty shells accumulate.
    fn index_remove(&mut self, t: Triple) {
        if let Some(by_obj) = self.obj_index.get_mut(&t.p) {
            if let Some(subjects) = by_obj.get_mut(&t.o) {
                subjects.remove(&t.s);
                if subjects.is_empty() {
                    by_obj.remove(&t.o);
                }
            }
            if by_obj.is_empty() {
                self.obj_index.remove(&t.p);
            }
        }
    }

    /// Insert an asserted (base) triple. Returns true iff fresh.
    pub fn assert(&mut self, t: Triple) -> bool {
        let fresh = self.by_pred.entry(t.p).or_default().insert((t.s, t.o));
        if fresh {
            self.index_insert(t);
        }
        fresh
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

    /// Build the full proof tree for `t` (SPEC-04 F4, acceptance #5).
    ///
    /// Recurses through the single-level [`Provenance`] recorded for each
    /// inferred triple. A triple with no proof entry is treated as asserted
    /// (a leaf). Derivation cycles are cut with a [`ProofTree::Cycle`] leaf.
    pub fn proof_tree(&self, t: &Triple) -> ProofTree {
        let mut path = FxHashSet::default();
        self.proof_tree_inner(t, &mut path)
    }

    fn proof_tree_inner(&self, t: &Triple, path: &mut FxHashSet<Triple>) -> ProofTree {
        let Some(prov) = self.proofs.get(t) else {
            return ProofTree::Asserted(*t);
        };
        if !path.insert(*t) {
            return ProofTree::Cycle(*t);
        }
        let premises = prov
            .premises
            .iter()
            .map(|p| self.proof_tree_inner(p, path))
            .collect();
        path.remove(t);
        ProofTree::Derived {
            triple: *t,
            rule_id: prov.rule_id,
            premises,
        }
    }

    /// Set the resolved max-cardinality restrictions. Called once at load
    /// time (`integration.rs`) or directly by tests.
    pub fn set_card_restrictions(&mut self, restrictions: Vec<MaxCardRestriction>) {
        self.card_restrictions = restrictions;
    }

    /// Set the resolved qualified max-cardinality restrictions. Called once at
    /// load time (`integration.rs`) or directly by tests.
    pub fn set_qual_card_restrictions(&mut self, restrictions: Vec<QualMaxCardRestriction>) {
        self.qual_card_restrictions = restrictions;
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
        if let (None, Some(oo)) = (s, o) {
            return match self.obj_index.get(&p).and_then(|by_obj| by_obj.get(&oo)) {
                Some(subjects) => Box::new(subjects.iter().map(move |&ss| Triple::new(ss, p, oo))),
                None => Box::new(std::iter::empty()),
            };
        }
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
            self.index_insert(t);
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
            self.index_remove(t);
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

    fn qual_card_restrictions(&self) -> &[QualMaxCardRestriction] {
        &self.qual_card_restrictions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::ProofTree;
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
    fn qual_card_restrictions_round_trip() {
        use crate::types::QualMaxCardRestriction;
        let mut s = store();
        assert!(s.qual_card_restrictions().is_empty());
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            filler: TermId(3),
            max: 1,
        }]);
        assert_eq!(s.qual_card_restrictions().len(), 1);
        assert_eq!(s.qual_card_restrictions()[0].filler, TermId(3));
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

    #[test]
    fn probe_object_bound_returns_mix_of_asserted_and_inferred() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.insert_inferred(
            t(4, 2, 3),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        s.assert(t(5, 2, 9)); // different object, must not show up
        let mut got: Vec<_> = s
            .probe(None, TermId(2), Some(TermId(3)))
            .map(|tr| tr.s)
            .collect();
        got.sort();
        assert_eq!(got, vec![TermId(1), TermId(4)]);
    }

    #[test]
    fn clear_inferred_prunes_obj_index_but_keeps_asserted_subject() {
        let mut s = store();
        // asserted and inferred triples share the same (p, o).
        s.assert(t(1, 2, 3));
        s.insert_inferred(
            t(4, 2, 3),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        let mut got: Vec<_> = s
            .probe(None, TermId(2), Some(TermId(3)))
            .map(|tr| tr.s)
            .collect();
        got.sort();
        assert_eq!(got, vec![TermId(1), TermId(4)]);

        s.clear_inferred();

        let got: Vec<_> = s
            .probe(None, TermId(2), Some(TermId(3)))
            .map(|tr| tr.s)
            .collect();
        assert_eq!(got, vec![TermId(1)]);
    }

    #[test]
    fn clear_inferred_removes_only_fully_inferred_object_bucket() {
        let mut s = store();
        // (p, o) bucket has only inferred subjects; after clear it must vanish
        // entirely (empty inner set / empty predicate map pruned), not linger
        // as an empty shell.
        s.insert_inferred(
            t(1, 2, 3),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        s.insert_inferred(
            t(4, 2, 3),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        s.clear_inferred();
        let got: Vec<_> = s.probe(None, TermId(2), Some(TermId(3))).collect();
        assert!(got.is_empty());
    }

    #[test]
    fn duplicate_object_multiple_subjects_survive_partial_removal() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.assert(t(5, 2, 3));
        s.insert_inferred(
            t(9, 2, 3),
            Provenance {
                rule_id: "r",
                premises: smallvec![],
            },
        );
        let mut got: Vec<_> = s
            .probe(None, TermId(2), Some(TermId(3)))
            .map(|tr| tr.s)
            .collect();
        got.sort();
        assert_eq!(got, vec![TermId(1), TermId(5), TermId(9)]);

        s.clear_inferred();

        let mut got: Vec<_> = s
            .probe(None, TermId(2), Some(TermId(3)))
            .map(|tr| tr.s)
            .collect();
        got.sort();
        assert_eq!(got, vec![TermId(1), TermId(5)]);
    }

    #[test]
    fn proof_tree_bottoms_out_at_asserted() {
        let mut s = store();
        let ab = t(1, 2, 3);
        let c1 = t(1, 4, 5);
        let c2 = t(1, 6, 7);
        s.assert(ab);
        s.insert_inferred(c1, Provenance::new("r1", [ab]));
        s.insert_inferred(c2, Provenance::new("r2", [c1]));

        let tree = s.proof_tree(&c2);
        match tree {
            ProofTree::Derived {
                triple,
                rule_id,
                premises,
            } => {
                assert_eq!(triple, c2);
                assert_eq!(rule_id, "r2");
                assert_eq!(premises.len(), 1);
                match &premises[0] {
                    ProofTree::Derived {
                        triple,
                        rule_id,
                        premises,
                    } => {
                        assert_eq!(*triple, c1);
                        assert_eq!(*rule_id, "r1");
                        assert_eq!(premises.len(), 1);
                        assert_eq!(premises[0], ProofTree::Asserted(ab));
                    }
                    other => panic!("expected Derived c1, got {other:?}"),
                }
            }
            other => panic!("expected Derived c2, got {other:?}"),
        }
        assert_eq!(s.proof_tree(&ab), ProofTree::Asserted(ab));
    }

    #[test]
    fn proof_tree_cuts_cycles() {
        let mut s = store();
        let x = t(1, 2, 3);
        let y = t(3, 2, 1);
        s.insert_inferred(x, Provenance::new("eq-sym", [y]));
        s.insert_inferred(y, Provenance::new("eq-sym", [x]));
        let tree = s.proof_tree(&x);
        match tree {
            ProofTree::Derived { premises, .. } => match &premises[0] {
                ProofTree::Derived { premises, .. } => {
                    assert_eq!(premises[0], ProofTree::Cycle(x));
                }
                other => panic!("expected nested Derived, got {other:?}"),
            },
            other => panic!("expected Derived, got {other:?}"),
        }
    }
}
