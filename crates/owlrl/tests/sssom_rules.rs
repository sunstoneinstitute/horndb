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
