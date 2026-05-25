//! Verify each Stage-1 rule fires correctly in isolation.

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::materialize;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

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
fn cls_hv1() {
    let (mut s, v) = fresh_store();
    let restriction = 70;
    let prop = 80;
    let val = 90;
    let u = 100;
    s.assert(t(restriction, v.owl_has_value.0, val));
    s.assert(t(restriction, v.owl_on_property.0, prop));
    s.assert(t(u, v.rdf_type.0, restriction));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(u, prop, val)));
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

#[test]
fn prp_fp() {
    // ?p rdf:type owl:FunctionalProperty ∧ ?x ?p ?y1 ∧ ?x ?p ?y2 ⇒ ?y1 owl:sameAs ?y2.
    // Then the closure backend symmetrises / transitive-closes the new sameAs.
    let (mut s, v) = fresh_store();
    let p = 50;
    let x = 100;
    let y1 = 200;
    let y2 = 201;
    s.assert(t(p, v.rdf_type.0, v.owl_functional_property.0));
    s.assert(t(x, p, y1));
    s.assert(t(x, p, y2));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    // prp-fp derivation: one direction is enough (eq-sym fills the other).
    assert!(
        s.contains(&t(y1, v.owl_same_as.0, y2)) || s.contains(&t(y2, v.owl_same_as.0, y1)),
        "prp-fp: expected y1 sameAs y2 (or its symmetric closure)",
    );
    // Closure backend symmetrises (eq-sym).
    assert!(s.contains(&t(y1, v.owl_same_as.0, y2)), "eq-sym");
    assert!(s.contains(&t(y2, v.owl_same_as.0, y1)), "eq-sym");
}

#[test]
fn prp_ifp() {
    // ?p rdf:type owl:InverseFunctionalProperty ∧ ?x1 ?p ?y ∧ ?x2 ?p ?y ⇒ ?x1 owl:sameAs ?x2.
    let (mut s, v) = fresh_store();
    let p = 50;
    let x1 = 100;
    let x2 = 101;
    let y = 200;
    s.assert(t(p, v.rdf_type.0, v.owl_inverse_functional_property.0));
    s.assert(t(x1, p, y));
    s.assert(t(x2, p, y));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(x1, v.owl_same_as.0, x2)), "prp-ifp + eq-sym");
    assert!(s.contains(&t(x2, v.owl_same_as.0, x1)), "prp-ifp + eq-sym");
    // Closure backend transitive-closes any chain (sanity: ?x1 sameAs ?x1 via trans through x2).
    // We don't assert eq-trans here because it would require a third individual.
}

#[test]
fn prp_rfp() {
    // ?p rdf:type owl:ReflexiveProperty ∧ ?x rdf:type owl:Thing ⇒ ?x ?p ?x.
    let (mut s, v) = fresh_store();
    let p = 50;
    let x = 100;
    s.assert(t(p, v.rdf_type.0, v.owl_reflexive_property.0));
    s.assert(t(x, v.rdf_type.0, v.owl_thing.0));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(x, p, x)), "prp-rfp: x p x");
}

#[test]
fn eq_rep_s() {
    // ?s owl:sameAs ?s' ∧ ?s ?p ?o ⇒ ?s' ?p ?o.
    let (mut s, v) = fresh_store();
    let s_a = 100;
    let s_b = 101;
    let p = 50;
    let o = 200;
    s.assert(t(s_a, v.owl_same_as.0, s_b));
    s.assert(t(s_a, p, o));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(s_b, p, o)), "eq-rep-s");
}

#[test]
fn eq_rep_p() {
    // ?p owl:sameAs ?p' ∧ ?s ?p ?o ⇒ ?s ?p' ?o.
    let (mut s, v) = fresh_store();
    let p1 = 50;
    let p2 = 51;
    let s_a = 100;
    let o = 200;
    s.assert(t(p1, v.owl_same_as.0, p2));
    s.assert(t(s_a, p1, o));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(s_a, p2, o)), "eq-rep-p");
}

#[test]
fn eq_rep_o() {
    // ?o owl:sameAs ?o' ∧ ?s ?p ?o ⇒ ?s ?p ?o'.
    let (mut s, v) = fresh_store();
    let o_a = 200;
    let o_b = 201;
    let s_a = 100;
    let p = 50;
    s.assert(t(o_a, v.owl_same_as.0, o_b));
    s.assert(t(s_a, p, o_a));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(s_a, p, o_b)), "eq-rep-o");
}
