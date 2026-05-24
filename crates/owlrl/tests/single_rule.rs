//! Verify each Stage-1 rule fires correctly in isolation.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::materialize;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

fn fresh_store() -> (MemStore, Vocabulary) {
    let v = Vocabulary::synthetic(10_000);
    (MemStore::new(v), v)
}

#[test]
fn cax_sco() {
    let (mut s, v) = fresh_store();
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(100, v.rdf_type.0, 1));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, 2)));
}

#[test]
fn prp_dom() {
    let (mut s, v) = fresh_store();
    let p = 50;
    let c = 60;
    s.assert(t(p, v.rdfs_domain.0, c));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, c)));
}

#[test]
fn prp_rng() {
    let (mut s, v) = fresh_store();
    let p = 50;
    let c = 60;
    s.assert(t(p, v.rdfs_range.0, c));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, v.rdf_type.0, c)));
}

#[test]
fn prp_symp() {
    let (mut s, v) = fresh_store();
    let p = 50;
    s.assert(t(p, v.rdf_type.0, v.owl_symmetric_property.0));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, p, 100)));
}

#[test]
fn prp_spo1() {
    let (mut s, v) = fresh_store();
    let p1 = 50;
    let p2 = 60;
    s.assert(t(p1, v.rdfs_sub_property_of.0, p2));
    s.assert(t(100, p1, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, p2, 200)));
}

#[test]
fn prp_inv1_and_inv2() {
    let (mut s, v) = fresh_store();
    let p1 = 50;
    let p2 = 60;
    s.assert(t(p1, v.owl_inverse_of.0, p2));
    s.assert(t(100, p1, 200));
    s.assert(t(300, p2, 400));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, p2, 100)), "inv1");
    assert!(s.contains(&t(400, p1, 300)), "inv2");
}

#[test]
fn cax_eqc_both_directions() {
    let (mut s, v) = fresh_store();
    s.assert(t(1, v.owl_equivalent_class.0, 2));
    s.assert(t(100, v.rdf_type.0, 1));
    s.assert(t(101, v.rdf_type.0, 2));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, 2)), "cax-eqc1");
    assert!(s.contains(&t(101, v.rdf_type.0, 1)), "cax-eqc2");
}
