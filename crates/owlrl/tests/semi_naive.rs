//! Verify the driver chains derivations across multiple rules and to fixed point.

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::materialize;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

#[test]
fn five_step_subclass_chain() {
    let v = Vocabulary::synthetic(10_000);
    let sco = v.rdfs_sub_class_of.0;
    let ty = v.rdf_type.0;
    let mut s = MemStore::new(v);
    // A ⊑ B ⊑ C ⊑ D ⊑ E ⊑ F, x : A
    for i in 1..=5 {
        s.assert(t(i, sco, i + 1));
    }
    s.assert(t(100, ty, 1));
    let mut b = RuleFiringBackend::new();
    let stats = materialize(&mut s, &mut b);
    for c in 2..=6 {
        assert!(s.contains(&t(100, ty, c)), "x : {c} missing");
    }
    assert!(
        stats.rounds <= 10,
        "should converge in ≤10 rounds, took {}",
        stats.rounds
    );
}

#[test]
fn domain_then_subclass() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let p = 50;
    let c1 = 60;
    let c2 = 70;
    s.assert(t(p, v.rdfs_domain.0, c1));
    s.assert(t(c1, v.rdfs_sub_class_of.0, c2));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    // prp-dom: 100 : c1. Then cax-sco: 100 : c2.
    assert!(s.contains(&t(100, v.rdf_type.0, c1)));
    assert!(s.contains(&t(100, v.rdf_type.0, c2)));
}

#[test]
fn fixed_point_is_actually_fixed() {
    // Re-running materialize after it converged must produce zero new triples.
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(2, v.rdfs_sub_class_of.0, 3));
    s.assert(t(100, v.rdf_type.0, 1));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    let stats2 = materialize(&mut s, &mut b);
    assert_eq!(
        stats2.triples_inferred, 0,
        "second materialize should be a no-op"
    );
}
