//! SPEC-29 D3 differential: `fork(load_base(S)).extend(D)` must equal
//! `load_base(S ∪ D)` as materialized sets, for the monotone OWL 2 RL rule
//! set. See `docs/specs/SPEC-29-named-graph-reasoning-scope.md` §D3.

use horndb_owlrl::Engine;
use std::collections::BTreeSet;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const OWL_FUNCTIONAL_PROPERTY: &str = "http://www.w3.org/2002/07/owl#FunctionalProperty";
const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
const OWL_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const OWL_DIFFERENT_FROM: &str = "http://www.w3.org/2002/07/owl#differentFrom";

type Triple = (String, String, String);

fn t(s: &str, p: &str, o: &str) -> Triple {
    (s.to_string(), p.to_string(), o.to_string())
}

/// Sorted `Vec` (not a `HashSet`) so a failing assertion prints a stable,
/// readable diff.
fn materialized_set(e: &Engine) -> BTreeSet<Triple> {
    e.materialized_triples()
        .expect("engine was loaded")
        .into_iter()
        .collect()
}

/// One curated-rule-shape fixture per the harness's `owl2-rl-50` subset:
/// `cax-sco`/`scm-sco` subclass chain, `prp-trp` transitive property,
/// `rdfs:domain`/`range`, `owl:inverseOf`, and `prp-fp` functional property —
/// all in one graph so a single split exercises every shape at once.
fn curated_fixture() -> Vec<Triple> {
    vec![
        // cax-sco / scm-sco: a two-step subclass chain.
        t("http://ex/A", RDFS_SUB_CLASS_OF, "http://ex/B"),
        t("http://ex/B", RDFS_SUB_CLASS_OF, "http://ex/C"),
        t("http://ex/i1", RDF_TYPE, "http://ex/A"),
        // prp-trp: transitive property.
        t("http://ex/p", RDF_TYPE, OWL_TRANSITIVE_PROPERTY),
        t("http://ex/n1", "http://ex/p", "http://ex/n2"),
        t("http://ex/n2", "http://ex/p", "http://ex/n3"),
        // rdfs:domain / rdfs:range.
        t("http://ex/q", RDFS_DOMAIN, "http://ex/Dom"),
        t("http://ex/q", RDFS_RANGE, "http://ex/Rng"),
        t("http://ex/i2", "http://ex/q", "http://ex/i3"),
        // owl:inverseOf.
        t("http://ex/r", OWL_INVERSE_OF, "http://ex/rInv"),
        t("http://ex/i4", "http://ex/r", "http://ex/i5"),
        // prp-fp: functional property collapses two objects to sameAs.
        t("http://ex/fp", RDF_TYPE, OWL_FUNCTIONAL_PROPERTY),
        t("http://ex/i6", "http://ex/fp", "http://ex/i7"),
        t("http://ex/i6", "http://ex/fp", "http://ex/i8"),
    ]
}

/// A tiny 5-line LCG (no `rand` dependency) — deterministic per seed, used
/// to assign each fixture triple to S or D.
fn lcg_bits(seed: u64, n: usize) -> Vec<bool> {
    let mut state = seed.wrapping_mul(2).wrapping_add(1); // avoid seed 0 fixed point
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) & 1 == 1
        })
        .collect()
}

/// Split `triples` into (S, D) by the LCG bits for `seed` — `true` sends a
/// triple to D, `false` to S. `S ∪ D == triples` as a set by construction.
fn split(triples: &[Triple], seed: u64) -> (Vec<Triple>, Vec<Triple>) {
    let bits = lcg_bits(seed, triples.len());
    let mut s = Vec::new();
    let mut d = Vec::new();
    for (triple, in_d) in triples.iter().zip(bits) {
        if in_d {
            d.push(triple.clone());
        } else {
            s.push(triple.clone());
        }
    }
    (s, d)
}

fn assert_fork_extend_equals_joint(triples: &[Triple], seed: u64) {
    let (s, d) = split(triples, seed);

    let mut base = Engine::new();
    base.load_base(s.clone()).unwrap();
    let mut forked = base.fork();
    forked.extend(d.clone()).unwrap();

    let mut joint = Engine::new();
    joint.load_base(triples.to_vec()).unwrap();

    assert_eq!(
        materialized_set(&forked),
        materialized_set(&joint),
        "seed {seed}: fork(load_base(S)).extend(D) != load_base(S ∪ D); S={s:?} D={d:?}"
    );
}

#[test]
fn fork_extend_equals_joint_load() {
    let fixture = curated_fixture();
    // A handful of deterministic splits, not one — including the
    // degenerate all-S and all-D corners.
    for seed in [0, 1, 2, 3, 7, 42] {
        assert_fork_extend_equals_joint(&fixture, seed);
    }
}

/// SPEC-29 D3 condition 1: full `owl:sameAs` materialization, no
/// representative canonicalization — an `eq-rep-s` derivation must cross the
/// fork boundary the same way it crosses any other split. The `sameAs`
/// assertion lands in S; the property use of one member of the pair lands in
/// D, so the derived `(b, p, v)` triple only appears if the spine's sameAs
/// axiom is honored by the extended (forked) engine.
#[test]
fn sameas_across_the_split() {
    let s = vec![t("http://ex/a", OWL_SAME_AS, "http://ex/b")];
    let d = vec![t("http://ex/a", "http://ex/p", "http://ex/v")];

    let mut base = Engine::new();
    base.load_base(s.clone()).unwrap();
    let mut forked = base.fork();
    forked.extend(d.clone()).unwrap();

    let mut joint = Engine::new();
    let mut all = s;
    all.extend(d);
    joint.load_base(all).unwrap();

    let forked_set = materialized_set(&forked);
    let joint_set = materialized_set(&joint);
    assert_eq!(
        forked_set, joint_set,
        "sameAs pair spanning S/D must materialize identically via fork/extend"
    );
    assert!(
        forked_set.contains(&t("http://ex/b", "http://ex/p", "http://ex/v")),
        "eq-rep-s should propagate the D-side property use across the S-side sameAs axiom"
    );
}

/// SPEC-29 D3 condition 2: inconsistency is a per-view flag, not a
/// store-wide halt. S alone is consistent; D introduces a disjoint-class
/// membership clash. The forked+extended engine must report inconsistent
/// while the un-extended template engine stays consistent.
#[test]
fn nothing_propagates_per_fork() {
    let s = vec![t("http://ex/C1", OWL_DISJOINT_WITH, "http://ex/C2")];
    let d = vec![
        t("http://ex/x", RDF_TYPE, "http://ex/C1"),
        t("http://ex/x", RDF_TYPE, "http://ex/C2"),
    ];

    let mut template = Engine::new();
    template.load_base(s).unwrap();
    assert!(
        template.is_consistent().unwrap(),
        "S alone must stay consistent"
    );

    let mut forked = template.fork();
    forked.extend(d).unwrap();
    assert!(
        !forked.is_consistent().unwrap(),
        "D's disjoint-class clash must make the forked view inconsistent"
    );
    assert!(
        template.is_consistent().unwrap(),
        "extending the fork must not mutate the template engine"
    );
}

/// `extend` with triples already present in the store must derive nothing
/// new — the materialized set is unchanged.
#[test]
fn extend_is_idempotent() {
    let triples = curated_fixture();
    let mut engine = Engine::new();
    engine.load_base(triples.clone()).unwrap();
    let before = materialized_set(&engine);

    engine.extend(triples).unwrap();
    let after = materialized_set(&engine);

    assert_eq!(
        before, after,
        "re-extending with already-present triples must not change the materialized set"
    );
}

/// Sanity check on the differences fixture: `owl:differentFrom` +
/// `owl:sameAs` on the same pair is another inconsistency shape (`eq-diff1`),
/// independent of the disjoint-classes shape above.
#[test]
fn different_from_and_same_as_clash_propagates_per_fork() {
    let s = vec![t("http://ex/a", OWL_DIFFERENT_FROM, "http://ex/b")];
    let d = vec![t("http://ex/a", OWL_SAME_AS, "http://ex/b")];

    let mut template = Engine::new();
    template.load_base(s).unwrap();
    assert!(template.is_consistent().unwrap());

    let mut forked = template.fork();
    forked.extend(d).unwrap();
    assert!(!forked.is_consistent().unwrap());
    assert!(template.is_consistent().unwrap());
}
