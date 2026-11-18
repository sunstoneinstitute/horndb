//! SPEC-11 F3 — SSSOM chaining rule conformance (RG3 / RI / RCE).
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

fn run(setup: impl FnOnce(&mut MemStore, &Vocabulary)) -> (MemStore, Vocabulary) {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    setup(&mut s, &v);
    materialize(&mut s, &mut RuleFiringBackend::new());
    (s, v)
}

/// SPEC-11 F3: RG1/RG2 are withdrawn. A store holding only OWL axioms
/// derives no mapping-predicate triple.
#[test]
fn owl_axioms_alone_derive_no_mapping_predicate() {
    let (a, b, c, d) = (TermId(1), TermId(2), TermId(3), TermId(4));
    let (s, v) = run(|s, v| {
        for x in [a, b, c, d] {
            s.assert(t(x, v.rdf_type, v.owl_class));
        }
        s.assert(t(a, v.rdfs_sub_class_of, b));
        s.assert(t(c, v.owl_equivalent_class, d));
    });
    for p in [
        v.skos_exact_match,
        v.skos_close_match,
        v.skos_broad_match,
        v.skos_narrow_match,
        v.skos_related_match,
    ] {
        assert_eq!(s.scan_predicate(p).count(), 0);
    }
}

#[test]
fn ri_narrow_inverts_to_broad() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_narrow_match, b));
    });
    assert!(s.contains(&t(b, v.skos_broad_match, a)));
}

#[test]
fn ri_broad_inverts_to_narrow() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_broad_match, b));
    });
    assert!(s.contains(&t(b, v.skos_narrow_match, a)));
}

#[test]
fn ri_cross_species_exact_is_symmetric() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.semapv_cross_species_exact_match, b));
    });
    assert!(s.contains(&t(b, v.semapv_cross_species_exact_match, a)));
}

#[test]
fn ri_cross_species_narrow_inverts_to_broad() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.semapv_cross_species_narrow_match, b));
    });
    assert!(s.contains(&t(b, v.semapv_cross_species_broad_match, a)));
}

#[test]
fn rce1_exact_then_broad_propagates_broad() {
    // A exactMatch B, B broadMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_exact_match, b));
        s.assert(t(b, v.skos_broad_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}

#[test]
fn t1_broad_match_is_transitive() {
    // A broadMatch B broadMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_broad_match, b));
        s.assert(t(b, v.skos_broad_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}

#[test]
fn t1_narrow_match_is_transitive() {
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_narrow_match, b));
        s.assert(t(b, v.skos_narrow_match, c));
    });
    assert!(s.contains(&t(a, v.skos_narrow_match, c)));
}

#[test]
fn rce2_broad_then_exact_propagates_broad() {
    // A broadMatch B, B exactMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_broad_match, b));
        s.assert(t(b, v.skos_exact_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}

#[test]
fn ri_skos_symmetric_mapping_predicates() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_exact_match, b));
        s.assert(t(a, v.skos_close_match, b));
        s.assert(t(a, v.skos_related_match, b));
    });
    assert!(s.contains(&t(b, v.skos_exact_match, a)));
    assert!(s.contains(&t(b, v.skos_close_match, a)));
    assert!(s.contains(&t(b, v.skos_related_match, a)));
}

#[test]
fn rg3_exact_match_generalises_to_close_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_exact_match, b));
    });
    assert!(s.contains(&t(a, v.skos_close_match, b)));
    assert!(
        !s.contains(&t(a, v.owl_same_as, b)),
        "ADR-0017: no sameAs bridge"
    );
}

/// Symmetry + exactMatch chaining entails self-matches (see the RI note in
/// rules.toml). Pinned so a change to this behaviour is deliberate.
#[test]
fn exact_match_symmetry_entails_self_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_exact_match, b));
    });
    for x in [a, b] {
        assert!(s.contains(&t(x, v.skos_exact_match, x)));
        assert!(s.contains(&t(x, v.skos_close_match, x)));
    }
}

/// SPEC-11 F3 RCE1/RCE2: a mapping chains across `owl:equivalentClass`, in
/// both positions, and keeps its own predicate.
#[test]
fn rce_chains_mappings_across_equivalent_class() {
    let (a, b, c) = (TermId(1), TermId(2), TermId(3));
    for p in [
        |v: &Vocabulary| v.skos_exact_match,
        |v: &Vocabulary| v.skos_broad_match,
        |v: &Vocabulary| v.skos_narrow_match,
    ] {
        // RCE1: A equivalentClass B, B p C  =>  A p C
        let (s, v) = run(|s, v| {
            s.assert(t(a, v.owl_equivalent_class, b));
            s.assert(t(b, p(v), c));
        });
        assert!(s.contains(&t(a, p(&v), c)));
        // RCE2: A p B, B equivalentClass C  =>  A p C
        let (s, v) = run(|s, v| {
            s.assert(t(a, p(v), b));
            s.assert(t(b, v.owl_equivalent_class, c));
        });
        assert!(s.contains(&t(a, p(&v), c)));
    }
}
