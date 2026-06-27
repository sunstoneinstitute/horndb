//! SPEC-11 F3 — SSSOM chaining rule conformance (RG / RI / RCE).
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

#[test]
fn rg1_equivalent_class_generalises_to_exact_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.owl_equivalent_class, b));
    });
    assert!(s.contains(&t(a, v.skos_exact_match, b)));
}

#[test]
fn rg2_subclass_generalises_to_broad_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.rdfs_sub_class_of, b));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, b)));
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
