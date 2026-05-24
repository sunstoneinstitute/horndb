//! Semi-naïve evaluation driver. Stage 1: full re-materialization only.

use crate::backend::ClosureBackend;
use crate::delta::Delta;
use crate::generated::{CompiledRule, RULES};
use crate::store::TripleStore;
use crate::types::TermId;
use crate::vocab::Vocabulary;
use rustc_hash::FxHashSet;

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub rounds: usize,
    pub triples_inferred: usize,
    pub rule_fires: usize,
}

/// Run forward chaining to fixed point. Does NOT clear existing inferred
/// triples — see `reset_and_materialize` for that.
pub fn materialize<S: TripleStore, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    let mut stats = Stats::default();
    // First round: every rule fires; treat all predicates as "dirty".
    let mut dirty: Option<FxHashSet<TermId>> = None;
    loop {
        stats.rounds += 1;
        let mut round_delta = Delta::new();

        // 1. Compiled rules.
        for rule in RULES {
            if rule.delegated {
                continue;
            }
            if !rule_relevant(rule, dirty.as_ref(), store.vocab()) {
                continue;
            }
            stats.rule_fires += 1;
            let d = (rule.fire)(store_as_dyn(store), &Delta::new());
            round_delta.merge(d);
        }

        // 2. Closure backend (handles delegated rules).
        let backend_delta = backend.close(store_as_dyn(store));
        round_delta.merge(backend_delta);

        // 3. Apply to store.
        let mut new_count = 0;
        let mut applied = Delta::new();
        for (t, prov) in round_delta.iter() {
            if store.insert_inferred(*t, prov.clone()) {
                new_count += 1;
                applied.insert(*t, prov.clone());
            }
        }
        stats.triples_inferred += new_count;

        if applied.is_empty() {
            break;
        }
        dirty = Some(applied.dirty_predicates());
    }
    stats
}

/// Drop all inferred triples and re-run forward chaining from the asserted base.
/// Implements SPEC-04 F7.
pub fn reset_and_materialize<S: TripleStore, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    store.clear_inferred();
    materialize(store, backend)
}

fn rule_relevant(
    rule: &CompiledRule,
    dirty: Option<&FxHashSet<TermId>>,
    vocab: &Vocabulary,
) -> bool {
    // First round (dirty = None): everything is relevant.
    let Some(dirty) = dirty else { return true };
    rule.body_predicates
        .iter()
        .any(|pa| dirty.contains(&pa(vocab)))
}

/// Coerce a generic `&S` to `&dyn TripleStore`. Needed because `RULES`
/// entries take `&dyn TripleStore`.
fn store_as_dyn<S: TripleStore>(s: &S) -> &dyn TripleStore {
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::RuleFiringBackend;
    use crate::store::MemStore;
    use crate::types::{TermId, Triple};
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn empty_store_terminates() {
        let v = Vocabulary::synthetic(1000);
        let mut store = MemStore::new(v);
        let mut backend = RuleFiringBackend::new();
        let stats = materialize(&mut store, &mut backend);
        assert_eq!(stats.triples_inferred, 0);
        assert!(stats.rounds >= 1);
    }

    #[test]
    fn cax_sco_two_hop() {
        let v = Vocabulary::synthetic(1000);
        let (sco, ty) = (v.rdfs_sub_class_of, v.rdf_type);
        let (a, b, c, x) = (TermId(1), TermId(2), TermId(3), TermId(4));
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C, x : A
        store.assert(t(a.0, sco.0, b.0));
        store.assert(t(b.0, sco.0, c.0));
        store.assert(t(x.0, ty.0, a.0));
        let mut backend = RuleFiringBackend::new();
        materialize(&mut store, &mut backend);
        assert!(
            store.contains(&t(x.0, ty.0, b.0)),
            "expected x : B (cax-sco)"
        );
        assert!(
            store.contains(&t(x.0, ty.0, c.0)),
            "expected x : C (cax-sco + scm-sco)"
        );
    }
}
