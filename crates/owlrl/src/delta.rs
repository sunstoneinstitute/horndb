//! Delta tables for semi-naïve evaluation.

use crate::provenance::Provenance;
use crate::types::{TermId, Triple};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default, Debug, Clone)]
pub struct Delta {
    triples: FxHashSet<Triple>,
    proofs: FxHashMap<Triple, Provenance>,
}

impl Delta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, t: Triple, prov: Provenance) -> bool {
        let fresh = self.triples.insert(t);
        if fresh {
            self.proofs.insert(t, prov);
        }
        fresh
    }

    pub fn contains(&self, t: &Triple) -> bool {
        self.triples.contains(t)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Triple, &Provenance)> {
        self.triples.iter().map(move |t| (t, &self.proofs[t]))
    }

    pub fn triples(&self) -> impl Iterator<Item = &Triple> {
        self.triples.iter()
    }

    pub fn len(&self) -> usize {
        self.triples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }

    /// Set of distinct predicate IDs touched by this delta.
    pub fn dirty_predicates(&self) -> FxHashSet<TermId> {
        self.triples.iter().map(|t| t.p).collect()
    }

    /// Merge `other` into `self`. Existing entries keep their original provenance.
    pub fn merge(&mut self, other: Delta) {
        let Delta {
            triples,
            mut proofs,
        } = other;
        for t in triples {
            if self.triples.insert(t) {
                if let Some(p) = proofs.remove(&t) {
                    self.proofs.insert(t, p);
                }
            }
        }
    }

    /// Drop any triples already present in `existing`.
    pub fn subtract(&mut self, existing: &FxHashSet<Triple>) {
        self.triples.retain(|t| !existing.contains(t));
        // `proofs` keys are a subset of `triples`, so the same predicate drops
        // exactly the proofs whose triples were just removed — no need to
        // snapshot the retained set.
        self.proofs.retain(|t, _| !existing.contains(t));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    fn prov(id: &'static str) -> Provenance {
        Provenance {
            rule_id: id,
            premises: smallvec![],
        }
    }

    #[test]
    fn insert_dedups() {
        let mut d = Delta::new();
        assert!(d.insert(t(1, 2, 3), prov("r1")));
        assert!(!d.insert(t(1, 2, 3), prov("r2")));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dirty_predicates_unique() {
        let mut d = Delta::new();
        d.insert(t(1, 2, 3), prov("r1"));
        d.insert(t(4, 2, 5), prov("r1"));
        d.insert(t(6, 7, 8), prov("r2"));
        let preds = d.dirty_predicates();
        assert_eq!(preds.len(), 2);
        assert!(preds.contains(&TermId(2)));
        assert!(preds.contains(&TermId(7)));
    }

    #[test]
    fn subtract_drops_known() {
        let mut d = Delta::new();
        d.insert(t(1, 2, 3), prov("r"));
        d.insert(t(4, 5, 6), prov("r"));
        let mut existing = FxHashSet::default();
        existing.insert(t(1, 2, 3));
        d.subtract(&existing);
        assert_eq!(d.len(), 1);
        assert!(d.contains(&t(4, 5, 6)));
    }
}
