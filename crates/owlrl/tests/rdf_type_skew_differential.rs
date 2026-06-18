//! Differential test for SPEC-04 F5 `rdf:type`-skew parallelism.
//!
//! The `Auto` (rayon, partition-by-class) and `Serial` strategies for the
//! `rdf:type`-driven list rules (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`)
//! must reach the *identical* materialised closure. `Serial` is the sequential
//! reference; `Auto` is the bounded-latency parallel replacement (issue #39).
//!
//! The fixtures deliberately push class extents above
//! `list_rules::PAR_TYPE_THRESHOLD` so the parallel branch is actually
//! exercised, then a proptest sweeps random schema+instance graphs.

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize_with, MaterializeOpts, ParallelStrategy};
use proptest::prelude::*;
use rustc_hash::FxHashSet;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

/// Materialise `base` with the given parallel strategy; return the full triple
/// set. `eq_rep_p` stays at its default (Optimized) so this test isolates the
/// F5 axis.
fn closure_with(base: &[Triple], parallel: ParallelStrategy) -> FxHashSet<Triple> {
    let v = Vocabulary::synthetic(10_000);
    let mut store = MemStore::new(v);
    store.assert_all(base.iter().copied());
    let mut backend = RuleFiringBackend::new();
    materialize_with(
        &mut store,
        &mut backend,
        MaterializeOpts {
            parallel,
            ..Default::default()
        },
    );
    store.all_triples()
}

fn assert_strategies_agree(base: &[Triple]) {
    let auto = closure_with(base, ParallelStrategy::Auto);
    let serial = closure_with(base, ParallelStrategy::Serial);
    assert_eq!(
        auto,
        serial,
        "Auto and Serial F5 strategies disagree.\nonly-in-auto={:?}\nonly-in-serial={:?}",
        auto.difference(&serial).collect::<Vec<_>>(),
        serial.difference(&auto).collect::<Vec<_>>(),
    );
}

/// `cls-int1` over a large intersection extent: `c = c1 ⊓ c2`, with many
/// subjects in `c1` and a subset also in `c2`. The `c1` extent is well above
/// the parallel threshold.
#[test]
fn cls_int1_large_extent() {
    let v = Vocabulary::synthetic(10_000);
    let (ty, sub) = (v.rdf_type.0, 9000u64); // 9000: the intersection class c
    let (c1, c2) = (9001u64, 9002u64);
    let n = 2_000u64;
    let mut base = Vec::new();
    // owl:intersectionOf list: c rdf:List (c1 c2)
    let (l0, l1) = (8000u64, 8001u64);
    base.push(t(sub, v.owl_intersection_of.0, l0));
    base.push(t(l0, v.rdf_first.0, c1));
    base.push(t(l0, v.rdf_rest.0, l1));
    base.push(t(l1, v.rdf_first.0, c2));
    base.push(t(l1, v.rdf_rest.0, v.rdf_nil.0));
    // n subjects in c1; every third also in c2.
    for i in 0..n {
        base.push(t(1_000_000 + i, ty, c1));
        if i % 3 == 0 {
            base.push(t(1_000_000 + i, ty, c2));
        }
    }
    assert_strategies_agree(&base);
}

/// `cls-uni` over a large union: `c = c1 ⊔ c2`, with many subjects spread
/// across both members.
#[test]
fn cls_uni_large_extent() {
    let v = Vocabulary::synthetic(10_000);
    let ty = v.rdf_type.0;
    let (cc, c1, c2) = (9100u64, 9101u64, 9102u64);
    let n = 2_000u64;
    let mut base = Vec::new();
    let (l0, l1) = (8100u64, 8101u64);
    base.push(t(cc, v.owl_union_of.0, l0));
    base.push(t(l0, v.rdf_first.0, c1));
    base.push(t(l0, v.rdf_rest.0, l1));
    base.push(t(l1, v.rdf_first.0, c2));
    base.push(t(l1, v.rdf_rest.0, v.rdf_nil.0));
    for i in 0..n {
        let member = if i % 2 == 0 { c1 } else { c2 };
        base.push(t(2_000_000 + i, ty, member));
    }
    assert_strategies_agree(&base);
}

/// `cax-adc` over a large disjoint-classes extent: many subjects in `c1`, a
/// subset also in `c2`, which makes them `owl:Nothing`.
#[test]
fn cax_adc_large_extent() {
    let v = Vocabulary::synthetic(10_000);
    let ty = v.rdf_type.0;
    let (adc, c1, c2) = (9200u64, 9201u64, 9202u64);
    let n = 2_000u64;
    let mut base = Vec::new();
    base.push(t(adc, ty, v.owl_all_disjoint_classes.0));
    let (l0, l1) = (8200u64, 8201u64);
    base.push(t(adc, v.owl_members.0, l0));
    base.push(t(l0, v.rdf_first.0, c1));
    base.push(t(l0, v.rdf_rest.0, l1));
    base.push(t(l1, v.rdf_first.0, c2));
    base.push(t(l1, v.rdf_rest.0, v.rdf_nil.0));
    for i in 0..n {
        base.push(t(3_000_000 + i, ty, c1));
        if i % 5 == 0 {
            base.push(t(3_000_000 + i, ty, c2));
        }
    }
    assert_strategies_agree(&base);
}

/// `prp-key` over a keyed class where many subject **pairs** share many values
/// of the single key property. This is the duplicate-heavy case: each shared
/// value re-derives the same `?x owl:sameAs ?y` head, so the parallel path must
/// dedup per-subject (rather than allocate one candidate per shared value) and
/// still land the identical closure as the serial path.
#[test]
fn prp_key_duplicate_heavy() {
    let v = Vocabulary::synthetic(10_000);
    let ty = v.rdf_type.0;
    let (c, key_p) = (9300u64, 9301u64);
    let n_subjects = 600u64; // > PAR_TYPE_THRESHOLD so the parallel branch runs
    let shared_vals = 8u64; // each pair shares this many key values
    let mut base = vec![t(c, v.owl_has_key.0, 8300)];
    // owl:hasKey list: (key_p)
    base.push(t(8300, v.rdf_first.0, key_p));
    base.push(t(8300, v.rdf_rest.0, v.rdf_nil.0));
    // Subjects are grouped in pairs; both members of a pair carry the SAME set
    // of `shared_vals` values on key_p, so prp-key derives `s0 sameAs s1` once
    // per shared value (the duplicate-heavy path).
    for pair in 0..(n_subjects / 2) {
        let s0 = 4_000_000 + pair * 2;
        let s1 = s0 + 1;
        base.push(t(s0, ty, c));
        base.push(t(s1, ty, c));
        for k in 0..shared_vals {
            let val = 5_000_000 + pair * shared_vals + k;
            base.push(t(s0, key_p, val));
            base.push(t(s1, key_p, val));
        }
    }
    assert_strategies_agree(&base);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Random small schema+instance graphs that exercise the list rules:
    /// an intersection, a union, and a disjoint-classes axiom over a tight
    /// class universe, plus random typed instances. Auto must equal Serial.
    #[test]
    fn auto_equals_serial_on_random_graphs(
        types in prop::collection::vec((1u64..=20, 0u64..=5), 0..60),
    ) {
        let v = Vocabulary::synthetic(10_000);
        let ty = v.rdf_type.0;
        // Fixed schema: classes 9001..=9006.
        let (ci_a, ci_b, ci_c) = (9001u64, 9002u64, 9003u64);
        let mut base = Vec::new();
        // c=9010 = ci_a ⊓ ci_b
        let (l0, l1) = (8000u64, 8001u64);
        base.push(t(9010, v.owl_intersection_of.0, l0));
        base.push(t(l0, v.rdf_first.0, ci_a));
        base.push(t(l0, v.rdf_rest.0, l1));
        base.push(t(l1, v.rdf_first.0, ci_b));
        base.push(t(l1, v.rdf_rest.0, v.rdf_nil.0));
        // c=9011 = ci_a ⊔ ci_c
        let (u0, u1) = (8010u64, 8011u64);
        base.push(t(9011, v.owl_union_of.0, u0));
        base.push(t(u0, v.rdf_first.0, ci_a));
        base.push(t(u0, v.rdf_rest.0, u1));
        base.push(t(u1, v.rdf_first.0, ci_c));
        base.push(t(u1, v.rdf_rest.0, v.rdf_nil.0));
        // AllDisjointClasses(ci_b, ci_c)
        let (d0, d1) = (8020u64, 8021u64);
        base.push(t(9020, ty, v.owl_all_disjoint_classes.0));
        base.push(t(9020, v.owl_members.0, d0));
        base.push(t(d0, v.rdf_first.0, ci_b));
        base.push(t(d0, v.rdf_rest.0, d1));
        base.push(t(d1, v.rdf_first.0, ci_c));
        base.push(t(d1, v.rdf_rest.0, v.rdf_nil.0));
        // Random typed instances over classes 9001..=9006.
        let class_of = |sel: u64| 9001 + sel; // 0..=5 -> 9001..=9006
        for (subj, csel) in types {
            base.push(t(100 + subj, ty, class_of(csel)));
        }
        assert_strategies_agree(&base);
    }
}
