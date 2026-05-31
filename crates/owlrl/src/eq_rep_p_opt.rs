//! Semantics-preserving specialised path for the OWL 2 RL `eq-rep-p` rule
//! (TASKS.md #2 / SPEC-04 F5 skew).
//!
//! `eq-rep-p` is `?p owl:sameAs ?p2 ∧ ?s ?p ?o ⇒ ?s ?p2 ?o`. The naïve
//! generated firing (`generated::fire_eq_rep_p`) loops every `owl:sameAs`
//! pair and re-scans the full extent of its subject-predicate every round.
//! For `k` mutually-sameAs predicates whose extents total `M` triples that
//! is `O(k·M)` candidate generation per round (most deduplicated away),
//! repeated until fixpoint.
//!
//! The output, however, is fixed: every predicate in an `owl:sameAs`
//! equivalence class ends up carrying the *union* of all members' extents.
//! This module computes that union once per class via union-find and writes
//! it to each member — `O(facts + output)`, no `k²` factor — yielding the
//! identical final closure (verified by `tests/eq_rep_p_differential.rs`).
//!
//! Union-find operates on the raw `owl:sameAs` edges, so it does not depend
//! on the closure backend having already materialised the symmetric/
//! transitive closure of `owl:sameAs`: connected components are derived
//! directly from the edge set. The optimized path may therefore derive some
//! class triples a round earlier than the naïve path, but both monotone
//! fixpoints are equal.

use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{TermId, Triple};
use rustc_hash::FxHashMap;

/// Minimal union-find over `TermId`. Path-compressed find; union by size.
#[derive(Default)]
struct UnionFind {
    parent: FxHashMap<TermId, TermId>,
    size: FxHashMap<TermId, u32>,
}

impl UnionFind {
    fn make(&mut self, x: TermId) {
        self.parent.entry(x).or_insert(x);
        self.size.entry(x).or_insert(1);
    }

    fn find(&mut self, x: TermId) -> TermId {
        let mut root = x;
        while self.parent[&root] != root {
            root = self.parent[&root];
        }
        // Path compression.
        let mut cur = x;
        while cur != root {
            let next = self.parent[&cur];
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }

    fn union(&mut self, a: TermId, b: TermId) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        let (sa, sb) = (self.size[&ra], self.size[&rb]);
        let (big, small) = if sa >= sb { (ra, rb) } else { (rb, ra) };
        self.parent.insert(small, big);
        self.size.insert(big, sa + sb);
    }
}

/// Compute the `eq-rep-p` closure for the current store in one pass over
/// `owl:sameAs` equivalence classes. Returns only triples not already
/// present in the store (the engine dedups again on apply, but checking
/// here keeps the returned `Delta` minimal).
pub fn fire_eq_rep_p_canonical(store: &dyn TripleStore) -> Delta {
    let v = store.vocab();
    let same_as = v.owl_same_as;
    let mut out = Delta::new();

    // 1. Build union-find over the owl:sameAs edge set.
    let mut uf = UnionFind::default();
    for t in store.scan_predicate(same_as) {
        uf.make(t.s);
        uf.make(t.o);
        uf.union(t.s, t.o);
    }
    if uf.parent.is_empty() {
        return out;
    }

    // 2. Group class members by representative root.
    let members: Vec<TermId> = uf.parent.keys().copied().collect();
    let mut classes: FxHashMap<TermId, Vec<TermId>> = FxHashMap::default();
    for m in members {
        let r = uf.find(m);
        classes.entry(r).or_default().push(m);
    }

    // 3. For each class, compute the union extent once, then write it to
    //    every member. `(s,o) -> source predicate` keeps a representative
    //    justification for provenance.
    for (_root, class) in classes {
        let mut union_extent: FxHashMap<(TermId, TermId), TermId> = FxHashMap::default();
        for &p in &class {
            for t in store.scan_predicate(p) {
                union_extent.entry((t.s, t.o)).or_insert(p);
            }
        }
        if union_extent.is_empty() {
            continue; // Pure-individual class with no predicate use: no work.
        }
        for &m in &class {
            for (&(s, o), &src) in &union_extent {
                if src == m {
                    continue; // Already present under m (it came from m).
                }
                let head = Triple::new(s, m, o);
                if store.contains(&head) || out.contains(&head) {
                    continue;
                }
                let mut premises = smallvec::SmallVec::<[Triple; 4]>::with_capacity(2);
                // Representative justification: (src sameAs m) ∧ (s src o).
                // The sameAs pair holds at fixpoint (eq-trans closes the
                // class); both premises are valid eq-rep-p antecedents.
                premises.push(Triple::new(src, same_as, m));
                premises.push(Triple::new(s, src, o));
                out.insert(
                    head,
                    Provenance {
                        rule_id: "eq-rep-p",
                        premises,
                    },
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn single_pair_substitutes_predicate() {
        let v = Vocabulary::synthetic(10_000);
        let mut store = MemStore::new(v);
        // p1 sameAs p2 ; (100 p1 200)
        store.assert(t(50, v.owl_same_as.0, 51));
        store.assert(t(100, 50, 200));
        let d = fire_eq_rep_p_canonical(&store);
        assert!(d.contains(&t(100, 51, 200)), "p1's extent must reach p2");
    }

    #[test]
    fn mutual_class_unions_extents() {
        let v = Vocabulary::synthetic(10_000);
        let mut store = MemStore::new(v);
        // p1 sameAs p2, p2 sameAs p3 (chain → one class {p1,p2,p3}).
        store.assert(t(50, v.owl_same_as.0, 51));
        store.assert(t(51, v.owl_same_as.0, 52));
        // distinct extents on each
        store.assert(t(1, 50, 2)); // in p1
        store.assert(t(3, 52, 4)); // in p3
        let d = fire_eq_rep_p_canonical(&store);
        // Every member carries the union {(1,2),(3,4)}.
        for &m in &[50u64, 51, 52] {
            assert!(d.contains(&t(1, m, 2)) || store.contains(&t(1, m, 2)));
            assert!(d.contains(&t(3, m, 4)) || store.contains(&t(3, m, 4)));
        }
    }
}
