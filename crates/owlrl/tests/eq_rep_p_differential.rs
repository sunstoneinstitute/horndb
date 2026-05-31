//! Differential test: the Optimized (class-canonical) and Naive (generated
//! nested-loop) `eq-rep-p` strategies must reach the identical materialised
//! closure. The Naive path is the W3C-conformant reference; Optimized is the
//! bounded-work replacement (TASKS.md #2 / SPEC-04 F5).

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize_with, EqRepPStrategy, MaterializeOpts};
use proptest::prelude::*;
use rustc_hash::FxHashSet;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

/// Materialise `base` with the given strategy; return the full triple set.
fn closure_with(base: &[Triple], strat: EqRepPStrategy) -> FxHashSet<Triple> {
    let v = Vocabulary::synthetic(10_000);
    let mut store = MemStore::new(v);
    store.assert_all(base.iter().copied());
    let mut backend = RuleFiringBackend::new();
    materialize_with(
        &mut store,
        &mut backend,
        MaterializeOpts { eq_rep_p: strat },
    );
    store.all_triples()
}

fn assert_strategies_agree(base: &[Triple]) {
    let opt = closure_with(base, EqRepPStrategy::Optimized);
    let naive = closure_with(base, EqRepPStrategy::Naive);
    assert_eq!(
        opt, naive,
        "Optimized and Naive eq-rep-p disagree.\nbase={base:?}\nonly-in-opt={:?}\nonly-in-naive={:?}",
        opt.difference(&naive).collect::<Vec<_>>(),
        naive.difference(&opt).collect::<Vec<_>>(),
    );
}

#[test]
fn adversarial_mutual_sameas_predicates() {
    // k predicates all sameAs each other, each with one distinct triple.
    // Naive does O(k^2) pairwise work; Optimized does one union. Both must
    // land the same closure (every predicate carries all k triples).
    let v = Vocabulary::synthetic(10_000);
    let same = v.owl_same_as.0;
    let k = 12u64;
    let preds: Vec<u64> = (100..100 + k).collect();
    let mut base = Vec::new();
    // chain sameAs so the closure backend forms one class
    for w in preds.windows(2) {
        base.push(t(w[0], same, w[1]));
    }
    // one distinct (s,o) per predicate
    for (i, &p) in preds.iter().enumerate() {
        base.push(t(1000 + i as u64, p, 2000 + i as u64));
    }
    assert_strategies_agree(&base);
}

#[test]
fn interaction_with_subproperty_and_type() {
    // sameAs predicate interacting with rdfs:subPropertyOf and rdf:type so
    // that eq-rep-p must re-fire across rounds.
    let v = Vocabulary::synthetic(10_000);
    let base = vec![
        t(50, v.owl_same_as.0, 51),          // p1 sameAs p2
        t(52, v.rdfs_sub_property_of.0, 50), // r ⊑ p1
        t(100, 52, 200),                     // (100 r 200) → prp-spo1 → (100 p1 200)
        t(60, v.rdfs_domain.0, 70),          // domain interplay
    ];
    assert_strategies_agree(&base);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Random small graphs over a tight term universe so sameAs classes and
    /// predicate reuse actually occur. Terms 1..=8; predicate selector maps
    /// onto owl:sameAs / rdfs:subPropertyOf / rdf:type / two plain preds so
    /// eq-* and prp-* interact with eq-rep-p.
    #[test]
    fn optimized_equals_naive_on_random_graphs(
        triples in prop::collection::vec(
            (1u64..=8, 0u64..=4, 1u64..=8),
            0..24,
        )
    ) {
        let v = Vocabulary::synthetic(10_000);
        let pred_of = |sel: u64| -> u64 {
            match sel {
                0 => v.owl_same_as.0,
                1 => v.rdfs_sub_property_of.0,
                2 => v.rdf_type.0,
                3 => 200, // plain predicate A
                _ => 201, // plain predicate B
            }
        };
        let base: Vec<Triple> = triples
            .into_iter()
            .map(|(s, psel, o)| t(s, pred_of(psel), o))
            .collect();
        assert_strategies_agree(&base);
    }
}
