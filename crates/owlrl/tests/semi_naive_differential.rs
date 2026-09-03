//! SPEC-15 fix #2 (HDB-40, #134): delta-driven semi-naïve firing of the
//! compiled rules must reach the identical closure, in the identical number
//! of rounds, as the naïve full-store re-join it replaces.
//!
//! `FiringStrategy::Naive` is the oracle. Corpora: the LUBM-shaped taxonomy
//! from `scripts/bench/gen_workload.py taxonomy` (the `audit-pass.sh` lubm
//! leg), the two skew bench corpora (`benches/eq_rep_p_skew.rs`,
//! `benches/rdf_type_skew.rs`), a hand-built mix that hits every compiled rule
//! family, and random graphs (proptest). The real LUBM-1 corpus is checked on
//! hornbench by `scripts/bench/seminaive-ab.sh` (`--dump-nt` under both
//! strategies, then `cmp`).

use horndb_owlrl::backend::{ClosureBackend, RuleFiringBackend};
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize_with, EqRepPStrategy, FiringStrategy, MaterializeOpts};
use proptest::prelude::*;
use rustc_hash::FxHashSet;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

fn vocab() -> Vocabulary {
    Vocabulary::synthetic(10_000)
}

/// Closure and round count of `base` under one firing strategy and one
/// closure backend.
fn closure_with_backend<B: ClosureBackend>(
    base: &[Triple],
    backend: &mut B,
    firing: FiringStrategy,
    eq_rep_p: EqRepPStrategy,
) -> (FxHashSet<Triple>, usize) {
    let mut store = MemStore::new(vocab());
    store.assert_all(base.iter().copied());
    let stats = materialize_with(
        &mut store,
        backend,
        MaterializeOpts {
            firing,
            eq_rep_p,
            ..Default::default()
        },
    );
    (store.all_triples(), stats.rounds)
}

/// Closure and round count of `base` under one firing strategy, on the
/// reference `RuleFiringBackend`.
fn closure_with(
    base: &[Triple],
    firing: FiringStrategy,
    eq_rep_p: EqRepPStrategy,
) -> (FxHashSet<Triple>, usize) {
    closure_with_backend(base, &mut RuleFiringBackend::new(), firing, eq_rep_p)
}

/// Both firing strategies must agree, under both `eq-rep-p` strategies (the
/// generated `fire_eq_rep_p` is a wildcard-predicate rule; `Naive` selects it).
fn assert_strategies_agree(name: &str, base: &[Triple]) {
    for eq_rep_p in [EqRepPStrategy::Optimized, EqRepPStrategy::Naive] {
        let (naive, naive_rounds) = closure_with(base, FiringStrategy::Naive, eq_rep_p);
        let (semi, semi_rounds) = closure_with(base, FiringStrategy::SemiNaive, eq_rep_p);
        if naive != semi {
            let missing: Vec<_> = naive.difference(&semi).collect();
            let extra: Vec<_> = semi.difference(&naive).collect();
            panic!(
                "{name} ({eq_rep_p:?}): semi-naïve closure differs from naïve\n\
                 missing from semi-naïve: {missing:?}\n\
                 extra in semi-naïve: {extra:?}"
            );
        }
        assert_eq!(
            naive_rounds, semi_rounds,
            "{name} ({eq_rep_p:?}): round count differs (naïve {naive_rounds}, semi-naïve {semi_rounds})"
        );
    }
}

// --- corpora -----------------------------------------------------------------

/// `gen_workload.py taxonomy DEPTH INST`: C0 ⊑ C1 ⊑ … ⊑ C{depth}, `instances`
/// individuals typed at C0.
fn taxonomy(v: &Vocabulary, depth: u64, instances: u64) -> Vec<Triple> {
    let mut base = Vec::new();
    for i in 0..depth {
        base.push(t(100 + i, v.rdfs_sub_class_of.0, 101 + i));
    }
    for j in 0..instances {
        base.push(t(1_000_000 + j, v.rdf_type.0, 100));
    }
    base
}

/// `benches/eq_rep_p_skew.rs::adversarial_base`: `k` mutually-sameAs
/// predicates, each carrying `rows` triples.
fn eq_rep_p_skew(v: &Vocabulary, k: u64, rows: u64) -> Vec<Triple> {
    let preds: Vec<u64> = (1_000..1_000 + k).collect();
    let mut base = Vec::new();
    for w in preds.windows(2) {
        base.push(t(w[0], v.owl_same_as.0, w[1]));
    }
    for (i, &p) in preds.iter().enumerate() {
        for r in 0..rows {
            base.push(t(10_000 + i as u64 * rows + r, p, 50_000 + r));
        }
    }
    base
}

/// `benches/rdf_type_skew.rs::skewed_base`: `c = c1 ⊓ … ⊓ c12` over a skewed
/// `c1` extent of `n` subjects, 90% of which are in every member class.
fn rdf_type_skew(v: &Vocabulary, n: u64) -> Vec<Triple> {
    let ty = v.rdf_type.0;
    let c = 9000u64;
    let members: Vec<u64> = (9001..9013).collect();
    let mut base = Vec::new();
    let list_base = 8000u64;
    base.push(t(c, v.owl_intersection_of.0, list_base));
    for (i, &m) in members.iter().enumerate() {
        let node = list_base + i as u64;
        let next = if i + 1 == members.len() {
            v.rdf_nil.0
        } else {
            node + 1
        };
        base.push(t(node, v.rdf_first.0, m));
        base.push(t(node, v.rdf_rest.0, next));
    }
    for i in 0..n {
        let subj = 1_000_000 + i;
        base.push(t(subj, ty, members[0]));
        if i % 10 != 0 {
            for &m in &members[1..] {
                base.push(t(subj, ty, m));
            }
        }
    }
    base
}

/// One hand-built graph that exercises every compiled rule family across
/// several rounds: class/property schema, restrictions, sameAs substitution
/// on all three positions, and the inconsistency markers.
fn rule_mix(v: &Vocabulary) -> Vec<Triple> {
    let ty = v.rdf_type.0;
    let (a, b, c, d) = (1, 2, 3, 4); // individuals
    let (c1, c2, c3, c4, restr) = (100, 101, 102, 103, 104); // classes
    let (p, q, r, inv) = (200, 201, 202, 203); // properties
    vec![
        // schema
        t(c1, v.rdfs_sub_class_of.0, c2),
        t(c2, v.rdfs_sub_class_of.0, c3),
        t(c3, v.owl_equivalent_class.0, c4),
        t(c1, v.rdf_type.0, v.owl_class.0),
        t(p, v.rdfs_domain.0, c1),
        t(p, v.rdfs_range.0, c2),
        t(q, v.rdfs_sub_property_of.0, p),
        t(r, v.owl_equivalent_property.0, q),
        t(p, v.owl_inverse_of.0, inv),
        t(q, v.rdf_type.0, v.owl_symmetric_property.0),
        t(r, v.rdf_type.0, v.owl_functional_property.0),
        t(p, v.rdf_type.0, v.owl_inverse_functional_property.0),
        t(c4, v.owl_disjoint_with.0, restr),
        // restriction: restr = ∀p.c3 and restr = p hasValue d
        t(restr, v.owl_all_values_from.0, c3),
        t(restr, v.owl_on_property.0, p),
        t(restr, v.owl_has_value.0, d),
        // instances
        t(a, q, b),
        t(b, r, c),
        t(b, r, d),
        t(c, ty, restr),
        t(a, v.owl_same_as.0, d),
        t(a, v.owl_different_from.0, b),
    ]
}

// --- tests -------------------------------------------------------------------

#[test]
fn taxonomy_depth_12() {
    let v = vocab();
    assert_strategies_agree("taxonomy d=12/2000", &taxonomy(&v, 12, 2_000));
}

#[test]
fn eq_rep_p_skew_corpus() {
    let v = vocab();
    assert_strategies_agree("eq_rep_p_skew k=8", &eq_rep_p_skew(&v, 8, 8));
}

#[test]
fn rdf_type_skew_corpus() {
    let v = vocab();
    assert_strategies_agree("rdf_type_skew n=2000", &rdf_type_skew(&v, 2_000));
}

/// Do not delete this test or the proptest below as "covered by the taxonomy
/// / skew corpora": mutating the codegen to drop one delta-bound variant is
/// caught *only* here and by `random_graphs_agree` (verified by mutation
/// during review). The single-family corpora still reach the same closure
/// with a variant missing.
#[test]
fn rule_mix_corpus() {
    let v = vocab();
    let base = rule_mix(&v);
    assert_strategies_agree("rule mix", &base);
    // Sanity: the mix really is multi-round and derives something in every
    // family, otherwise the parity check above proves little.
    let (closure, rounds) =
        closure_with(&base, FiringStrategy::SemiNaive, EqRepPStrategy::Optimized);
    assert!(
        rounds >= 3,
        "expected a multi-round derivation, got {rounds}"
    );
    assert!(
        closure.contains(&t(1, v.rdf_type.0, 103)),
        "cax-sco/eqc chain"
    );
    assert!(closure.contains(&t(2, 203, 1)), "prp-spo1 + prp-inv1");
    assert!(closure.contains(&t(3, v.owl_same_as.0, 4)), "prp-fp");
    assert!(
        closure.contains(&t(1, v.rdf_type.0, v.owl_nothing.0)),
        "eq-diff1"
    );
}

/// Random schema + instance graphs over a tight universe so the rules chain.
fn random_base() -> impl Strategy<Value = Vec<Triple>> {
    let v = vocab();
    let cls = 100u64..106;
    let prop = 200u64..204;
    let ind = 1u64..8;
    let schema = prop_oneof![
        (cls.clone(), cls.clone()).prop_map(move |(a, b)| vec![t(a, v.rdfs_sub_class_of.0, b)]),
        (cls.clone(), cls.clone()).prop_map(move |(a, b)| vec![t(a, v.owl_equivalent_class.0, b)]),
        (cls.clone(), cls.clone()).prop_map(move |(a, b)| vec![t(a, v.owl_disjoint_with.0, b)]),
        (prop.clone(), cls.clone()).prop_map(move |(p, c)| vec![t(p, v.rdfs_domain.0, c)]),
        (prop.clone(), cls.clone()).prop_map(move |(p, c)| vec![t(p, v.rdfs_range.0, c)]),
        (prop.clone(), prop.clone()).prop_map(move |(a, b)| vec![t(
            a,
            v.rdfs_sub_property_of.0,
            b
        )]),
        (prop.clone(), prop.clone()).prop_map(move |(a, b)| vec![t(a, v.owl_inverse_of.0, b)]),
        (prop.clone(), prop.clone()).prop_map(move |(a, b)| vec![t(
            a,
            v.owl_equivalent_property.0,
            b
        )]),
        (prop.clone(), prop.clone()).prop_map(move |(a, b)| vec![t(a, v.owl_same_as.0, b)]),
        prop.clone()
            .prop_map(move |p| vec![t(p, v.rdf_type.0, v.owl_symmetric_property.0)]),
        prop.clone()
            .prop_map(move |p| vec![t(p, v.rdf_type.0, v.owl_transitive_property.0)]),
        prop.clone()
            .prop_map(move |p| vec![t(p, v.rdf_type.0, v.owl_functional_property.0)]),
        prop.clone().prop_map(move |p| vec![t(
            p,
            v.rdf_type.0,
            v.owl_inverse_functional_property.0
        )]),
        (cls.clone(), prop.clone(), cls.clone()).prop_map(move |(x, p, y)| {
            vec![
                t(x, v.owl_all_values_from.0, y),
                t(x, v.owl_on_property.0, p),
            ]
        }),
        (cls.clone(), prop.clone(), ind.clone()).prop_map(move |(x, p, y)| {
            vec![t(x, v.owl_has_value.0, y), t(x, v.owl_on_property.0, p)]
        }),
    ];
    let instance = prop_oneof![
        (ind.clone(), cls).prop_map(move |(i, c)| t(i, v.rdf_type.0, c)),
        (ind.clone(), prop, ind.clone()).prop_map(|(s, p, o)| t(s, p, o)),
        (ind.clone(), ind).prop_map(move |(a, b)| t(a, v.owl_same_as.0, b)),
    ];
    (
        prop::collection::vec(schema, 0..10),
        prop::collection::vec(instance, 0..20),
    )
        .prop_map(|(s, i)| s.into_iter().flatten().chain(i).collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn random_graphs_agree(base in random_base()) {
        assert_strategies_agree("random", &base);
    }
}

/// Same rule-family mix under the GraphBLAS closure backend
/// (`BackendChoice::GraphBlas`). The backend feeds its own closure delta back
/// into the compiled rules each round, so it is a second, differently-shaped
/// delta source for the semi-naïve loop and needs its own parity check.
#[cfg(feature = "graphblas-backend")]
#[test]
fn rule_mix_corpus_graphblas() {
    use horndb_owlrl::graphblas_backend::GraphBlasBackend;

    let v = vocab();
    let base = rule_mix(&v);
    for eq_rep_p in [EqRepPStrategy::Optimized, EqRepPStrategy::Naive] {
        let (naive, naive_rounds) = closure_with_backend(
            &base,
            &mut GraphBlasBackend::new(),
            FiringStrategy::Naive,
            eq_rep_p,
        );
        let (semi, semi_rounds) = closure_with_backend(
            &base,
            &mut GraphBlasBackend::new(),
            FiringStrategy::SemiNaive,
            eq_rep_p,
        );
        assert_eq!(
            naive, semi,
            "graphblas rule mix ({eq_rep_p:?}): semi-naive closure differs from naive"
        );
        assert_eq!(
            naive_rounds, semi_rounds,
            "graphblas rule mix ({eq_rep_p:?}): round counts differ"
        );
        assert!(
            closure_contains_chain(&semi, &v),
            "graphblas rule mix ({eq_rep_p:?}): the mix derived nothing interesting"
        );
    }
}

/// The one derivation every backend must reach on `rule_mix`: the
/// `cax-sco`/`cax-eqc` chain. Guards the GraphBLAS parity test against
/// passing on two identically-empty closures.
#[cfg(feature = "graphblas-backend")]
fn closure_contains_chain(closure: &FxHashSet<Triple>, v: &Vocabulary) -> bool {
    closure.contains(&t(1, v.rdf_type.0, 103))
}
